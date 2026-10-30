"""Headed Chromium proof of one desktop with relay and private HTTPS routes.

Requires this Mac to be connected to Tailscale with HTTPS enabled. The relay edge
is a local byte-forwarding substitute behind a real, trusted Tailscale certificate.
The daemon, direct Serve owner, pairing tickets, QR decoder, E2EE and PTY are real.
Browser route fault injection closes native sockets; it never fakes authentication.
No certificate bypass, background Serve, Funnel or global Serve reset is used.
"""
import argparse
import binascii
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tempfile
from urllib.parse import parse_qs, urlencode, urlsplit
import zlib

from mobile_browser import until


ROUTER = r"""
const http=require('node:http'),net=require('node:net'),fs=require('node:fs'),path=require('node:path');
const [root,targetFile,ready,hash]=process.argv.slice(2),peers=new Set();let blocked=false;
const server=http.createServer((req,res)=>{
 const u=new URL(req.url,'http://localhost');res.setHeader('Cache-Control','no-store');
 if(u.pathname==='/probe'){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({relaySockets:peers.size,blocked}));return;}
 if(u.pathname==='/relay-deny'){blocked=true;for(const socket of peers)socket.end(Buffer.from([0x88,0x02,0x03,0xf0]));res.end('ok');return;}
 if(u.pathname==='/relay-off'||u.pathname==='/relay-on'){blocked=u.pathname==='/relay-off';if(blocked)for(const s of peers)s.destroy();res.end('ok');return;}
 if(u.pathname.startsWith('/r/')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({relay:true,hash}));return;}
 const file=u.pathname.replace(/^\/m\/[0-9a-f]{16}\//,'').replace(/^\//,'')||'index.html',resolved=path.resolve(root,file);
 if(!resolved.startsWith(root+path.sep)){res.writeHead(403);res.end();return;}
 try{const data=fs.readFileSync(resolved);res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.html')?'text/html':file.endsWith('.png')?'image/png':file.endsWith('.svg')?'image/svg+xml':'application/json');
 if(file==='index.html')res.setHeader('Content-Security-Policy',"default-src 'self'; connect-src 'self' wss://*.trycloudflare.com wss://*.ts.net:* https://errors.krill.page; img-src 'self' blob: data:; script-src 'self'; style-src 'self'; base-uri 'none'; frame-ancestors 'none'");
 res.end(data);}catch{res.writeHead(404);res.end();}
});
server.on('upgrade',(req,socket,head)=>{
 if(blocked||!fs.existsSync(targetFile)){socket.destroy();return;}
 const route=new URL(fs.readFileSync(targetFile,'utf8')),upstream=net.connect(Number(route.port),route.hostname,()=>{
  peers.add(socket);const headers={...req.headers,host:route.host,origin:route.origin};
  upstream.write('GET /ws HTTP/1.1\r\n'+Object.entries(headers).map(([k,v])=>k+': '+v).join('\r\n')+'\r\n\r\n');
  if(head.length)upstream.write(head);socket.pipe(upstream);upstream.pipe(socket);
 });upstream.on('error',()=>socket.destroy());socket.on('error',()=>upstream.destroy());socket.on('close',()=>{peers.delete(socket);upstream.destroy();});
});
server.listen(0,'127.0.0.1',()=>fs.writeFileSync(ready,String(server.address().port)));
"""

INSTRUMENT = r"""
localStorage.setItem('krill-mobile-ui-v1',JSON.stringify({language:'en',installOffered:true}));
globalThis.dualProbe={epoch:crypto.randomUUID(),sockets:[],blocked:[],violations:[],notices:[]};
addEventListener('securitypolicyviolation',e=>dualProbe.violations.push({directive:e.effectiveDirective,uri:e.blockedURI}));
const NativeWebSocket=WebSocket;
globalThis.WebSocket=class extends NativeWebSocket{
 constructor(...args){super(...args);this.probeKind=new URL(this.url).pathname.startsWith('/r/')?'relay':'tailscale';dualProbe.sockets.push(this);
 this.addEventListener('message',e=>{if(this.dropReply){this.dropReply=false;e.stopImmediatePropagation();dualProbe.dropped=this;this.close(4000,'test dropped reply');}});
 if(dualProbe.blocked.includes(this.probeKind))queueMicrotask(()=>this.close());}
 send(data){if(dualProbe.dropNextSend){dualProbe.dropNextSend=false;this.dropReply=true;}return super.send(data);}
};
addEventListener('DOMContentLoaded',()=>new MutationObserver(()=>{const n=document.getElementById('notice');if(n&&!n.hidden&&n.textContent)dualProbe.notices.push(n.textContent);}).observe(document.body,{subtree:true,childList:true,characterData:true,attributes:true}));
"""


def qr_photo(output, destination):
    plain = re.sub(r'\x1b\[[0-9;]*m', '', output)
    rows = []
    for line in plain.splitlines():
        if len(line) != 65 or any(c not in ' █▀▄' for c in line):
            break
        rows.extend([[0 if c in dark else 255 for c in line] for dark in ('█▀', '█▄')])
    assert len(rows) == 66, 'expected the product version-10 QR including quiet zone'
    pixels = b''.join((b'\0' + bytes(v for v in row for _ in range(8))) * 8 for row in rows)

    def chunk(kind, body):
        return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', binascii.crc32(kind + body))

    destination.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 520, 528, 8, 0, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))
    return next(line for line in plain.splitlines() if line.startswith(('http://', 'https://')))


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tailscale-bin', default='/Applications/Tailscale.app/Contents/MacOS/Tailscale')
    parser.add_argument('--https-port', type=int, default=18343)
    parser.add_argument('--order', action='append', choices=['relay-first', 'tailscale-first', 'blocked-relay-first'])
    parser.add_argument('--screenshots', default='/tmp/krill-dual-route-browser')
    args = parser.parse_args()
    tests = Path(__file__).resolve().parent
    root = tests.parents[2]
    binaries = Path(os.environ.get('KRILL_MOBILE_TEST_BIN', str(Path(os.environ.get('CARGO_TARGET_DIR', '/Volumes/krill-build/targets/shared')) / 'debug')))
    shots = Path(args.screenshots).resolve()
    shots.mkdir(parents=True, exist_ok=True)
    ts = args.tailscale_bin
    ts_env = dict(os.environ, TAILSCALE_BE_CLI='1')
    fixture_env = {key: value for key, value in os.environ.items()
                   if key not in ('TERM', 'SHLVL', 'TERM_PROGRAM', 'PS1', 'TAILSCALE_BE_CLI')}

    def ts_json(*arguments):
        return json.loads(subprocess.check_output([ts, *arguments], env=ts_env, text=True, timeout=15))

    before = ts_json('serve', 'status', '--json')
    status = ts_json('status', '--json')
    assert status['BackendState'] == 'Running', 'Tailscale must already be running'
    dns = status['Self']['DNSName'].rstrip('.')
    ip = next(value for value in status['Self']['TailscaleIPs'] if '.' in value)
    assert args.https_port > 1024 and args.https_port != 443

    def ports(config):
        result = {int(value) for value in config.get('TCP', {})}
        for kind in ('Web', 'AllowFunnel'):
            result.update(int(authority.rsplit(':', 1)[1]) for authority in config.get(kind, {}) if ':' in authority and authority.rsplit(':', 1)[1].isdigit())
        for child in config.get('Foreground', {}).values():
            result.update(ports(child))
        return result

    assert args.https_port not in ports(before), 'requested test HTTPS port already has a Serve owner'
    origin = f'https://{dns}:{args.https_port}'
    mobile_hash = subprocess.check_output(['python3', str(root / 'packaging/release_check.py'), 'mobile-hash'], cwd=root, text=True).strip()
    base = f'{origin}/m/{mobile_hash}/'
    owned = []
    sessions = []
    observations = {'relay_substitute': origin, 'mobile_hash': mobile_hash, 'orders': []}
    with tempfile.TemporaryDirectory(prefix='krill-dual-route-') as directory:
        scratch = Path(directory)
        router = scratch / 'router.cjs'
        router.write_text(ROUTER)
        target = scratch / 'target'
        ready = scratch / 'port'
        instrument = scratch / 'instrument.js'
        instrument.write_text(INSTRUMENT)
        try:
            proxy = subprocess.Popen(['node', str(router), str(tests.parent / 'src/mobile/assets'), str(target), str(ready), mobile_hash], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            owned.append(proxy)
            until(lambda: ready.exists() or proxy.poll() is not None)
            assert ready.exists(), proxy.stderr.read().decode()
            serve_log = (shots / 'edge-serve.log').open('w')
            edge = subprocess.Popen([ts, 'serve', f'--https={args.https_port}', f'http://127.0.0.1:{ready.read_text()}'], env=ts_env, stdin=subprocess.DEVNULL, stdout=serve_log, stderr=serve_log)
            owned.append(edge)

            def trusted_ready():
                return subprocess.run(['curl', '--silent', '--fail', '--noproxy', '*', '--max-time', '3', origin + '/probe'], capture_output=True).returncode == 0

            until(trusted_ready, 45)
            fixture_log = (shots / 'fixture.log').open('w')
            metadata = scratch / 'fixture.json'
            fixture = subprocess.Popen(['node', str(tests / 'mobile_pwa.cjs'), str(binaries / 'krilld'), '--keep'],
                                       env=dict(fixture_env, M9_BROWSER_METADATA=str(metadata), M9_BROWSER_ADDRESS='127.0.0.1:0', KRILL_MOBILE_ORIGIN=origin, KRILL_TAILSCALE_BIN=ts), stdout=fixture_log, stderr=fixture_log)
            owned.append(fixture)
            until(lambda: metadata.exists() or fixture.poll() is not None, 120)
            assert metadata.exists(), (shots / 'fixture.log').read_text()
            config = json.loads(metadata.read_text())
            target.write_text(config['base'])
            env = dict(os.environ, KRILL_STATE_DIR=config['dir'])
            sid = config['session']

            def cli(*command):
                return subprocess.check_output([str(binaries / 'krill'), *command], env=env, text=True, timeout=60)

            def offer(direct=False):
                command = ('mobile', 'pair', '--tailscale', '--address', ip + ':' + str(urlsplit(config['base']).port)) if direct else ('mobile', 'pair', '--address', config['base'].split('://')[1])
                return json.loads(cli(*command, '--json'))['PairingOffer']['url']

            def relay_link():
                # A genuine daemon ticket travels through the byte-forwarding relay substitute.
                link = urlsplit(offer())
                fragment = parse_qs(link.fragment)
                intent = json.loads((Path(config['dir']) / 'mobile-gateway.json').read_text())
                fragment.update(r=[intent['host_id']], h=[intent['host_id']])
                fragment.pop('d', None)
                return base + '#' + urlencode({key: value[0] for key, value in fragment.items()})

            def device_count():
                return len(json.loads((Path(config['dir']) / 'mobile-devices.json').read_text()))

            orders = list(dict.fromkeys(args.order or ('blocked-relay-first', 'relay-first', 'tailscale-first')))
            orders.sort(key=lambda order: order != 'blocked-relay-first')
            for order in orders:
                session = f'krill-dual-{os.getpid()}-{order}'
                sessions.append(session)

                def browser(*command):
                    result = subprocess.run(['agent-browser', '--session', session, *command], capture_output=True, text=True, timeout=60)
                    assert result.returncode == 0, result.stdout + result.stderr
                    return result.stdout

                def evaluate(script):
                    return json.loads(browser('eval', '--json', script))['data']['result']

                def wait(script):
                    browser('wait', '--fn', script)

                def click(name):
                    browser('find', 'role', 'button', 'click', '--name', name, '--exact')

                def shot(name):
                    browser('screenshot', str(shots / f'{order}-{name}.png'))

                def saved():
                    return evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1"))')

                def ready_terminal():
                    wait(f'KrillMobile.store.connection.phase==="ready" && KrillMobile.store.sessions.has({sid})')
                    evaluate(f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({sid}).groupKey)+"/s/{sid}");true')
                    wait('!!KrillMobile.activeTerminal && document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("READY")')
                    evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')

                start = offer(True) if order == 'tailscale-first' else relay_link()
                assert start.startswith(base), start.split('#')[0]
                browser('--headed', '--init-script', str(instrument), 'open', start)
                browser('set', 'viewport', '393', '852')
                wait('KrillMobile.store.connection.phase==="ready"')
                wait(f'KrillMobile.store.sessions.has({sid})')
                assert evaluate('isSecureContext') is True
                assert evaluate('location.origin') == origin
                first = saved()
                count = device_count()
                epoch = evaluate('dualProbe.epoch')
                ready_terminal()
                browser('fill', '.command-input', 'retained draft ' + order)
                session_route = evaluate('location.hash')
                evaluate('KrillMobile.app.navigate("/");true')
                wait('!!document.querySelector(".host-card")')
                if order == 'blocked-relay-first':
                    evaluate('(async()=>{await fetch("/relay-deny");return true})()')
                    wait('KrillMobile.app.transport.blocked')
                    evaluate('globalThis.blockedOwner=KrillMobile.app.transport;dualProbe.blocked=["relay"];true')
                    observations['blocked_before_attach'] = True
                live_before = evaluate('dualProbe.sockets.filter(s=>s.readyState===1).map(s=>dualProbe.sockets.indexOf(s))')
                if order != 'tailscale-first':
                    qr_log = (scratch / 'qr.log').open('w')
                    qr = subprocess.Popen([str(binaries / 'krill'), 'mobile', 'pair', '--tailscale', '--address', ip + ':' + str(urlsplit(config['base']).port)], env=env, stdout=qr_log, stderr=qr_log)
                    owned.append(qr)
                    until(lambda: 'Waiting for pairing' in (scratch / 'qr.log').read_text() or qr.poll() is not None, 60)
                    photo = shots / 'tailscale-real-qr.png'
                    qr_photo((scratch / 'qr.log').read_text(), photo)
                    click('Pair Desktop')
                    wait('!!document.querySelector("dialog.scanner")')
                    browser('upload', 'dialog.scanner input[type=file]', str(photo))
                    wait('dualProbe.notices.some(text=>text.includes("Desktop backup added"))')
                    stop(qr)
                    qr_log.close()
                else:
                    link = relay_link()
                    click('Pair Desktop')
                    click('Paste link instead')
                    browser('fill', 'input[type=url]', link)
                    click('Continue')
                    wait('dualProbe.notices.some(text=>text.includes("Desktop backup added"))')
                if order == 'blocked-relay-first':
                    assert not evaluate('KrillMobile.app.transport.blocked'), 'an authenticated backup left the retained owner blocked'
                    wait('KrillMobile.app.transport.ready')
                    assert evaluate('KrillMobile.app.transport===blockedOwner'), 'route recovery replaced the RPC owner'
                    evaluate('(async()=>{await fetch("/relay-on");dualProbe.blocked=[];return true})()')
                second = saved()
                assert len(second['hosts']) == 1, 'adding a route created another desktop profile'
                identity = lambda book: [(h['id'], h['device_id'], h['credential']) for h in book['hosts']]
                assert identity(first) == identity(second), 'adding a route replaced the saved identity'
                assert device_count() == count, 'adding a route created another device credential'
                assert second['hosts'][0]['routes'].get('tailscale') and second['hosts'][0]['routes'].get('relay'), 'both authenticated route descriptors must remain saved'
                assert evaluate('dualProbe.epoch') == epoch, 'adding a route reloaded the page'
                assert evaluate('location.origin') == origin
                assert evaluate(f'{json.dumps(live_before)}.every(i=>dualProbe.sockets[i].readyState===1)'), 'adding a route closed the healthy main transport'
                shot('both-routes')
                ready_terminal()
                assert evaluate('location.hash') == session_route
                assert evaluate('document.querySelector(".command-input").value') == 'retained draft ' + order
                active_kind = 'relay' if order == 'relay-first' else 'tailscale'
                other_kind = 'tailscale' if active_kind == 'relay' else 'relay'
                evaluate(f'dualProbe.blocked=[{json.dumps(active_kind)}];dualProbe.sockets.filter(s=>s.probeKind==={json.dumps(active_kind)}&&s.readyState<2).forEach(s=>s.close(4000,"route fault"));true')
                wait(f'KrillMobile.store.connection.phase==="ready" && dualProbe.sockets.some(s=>s.probeKind==={json.dumps(other_kind)}&&s.readyState===1)')
                wait('!!KrillMobile.activeTerminal && document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("READY")')
                assert evaluate('location.hash') == session_route
                assert evaluate('document.querySelector(".command-input").value') == 'retained draft ' + order
                command = 'dual-route-once-' + order
                browser('fill', '.command-input', command)
                evaluate('document.querySelector(".buffered-command").requestSubmit();true')
                until(lambda: 'got:' + command in cli('read', str(sid)))
                assert cli('read', str(sid)).count('got:' + command) == 1
                shot('fallback-terminal')
                click('Terminal actions')
                click('Send prompt')
                mutation = 'dual-unknown-once-' + order
                browser('fill', '#prompt', mutation)
                evaluate('dualProbe.blocked=[];dualProbe.dropNextSend=true;document.getElementById("composer").requestSubmit();true')
                wait('dualProbe.dropped?.readyState===3 && KrillMobile.store.connection.phase==="ready"')
                until(lambda: 'got:' + mutation in cli('read', str(sid)))
                assert cli('read', str(sid)).count('got:' + mutation) == 1
                assert evaluate('KrillMobile.app.transport.updateBusy'), 'uncertain mutation guard was cleared'
                assert any('not resent' in text for text in evaluate('dualProbe.notices'))
                assert evaluate('document.getElementById("prompt").value') == mutation
                click('Cancel')
                assert evaluate('dualProbe.violations') == [], evaluate('dualProbe.violations')
                observations['orders'].append({'order': order, 'profiles': len(second['hosts']), 'device_count_unchanged': True, 'same_origin': True, 'fallback': other_kind, 'pty_input_count': 1, 'uncertain_rpc_input_count': 1, 'csp_violations': [], 'blocked_owner_recovered': order == 'blocked-relay-first'})
                shot('no-replay')
                browser('close')
            print('PASS trusted HTTPS Chromium: relay→Tailscale QR photo, Tailscale→relay paste, one profile/credential, preserved draft/session, blocked relay owner recovery, route fallback both ways, PTY input once, lost RPC reply not replayed, CSP', flush=True)
        except BaseException:
            for session in sessions:
                subprocess.run(['agent-browser', '--session', session, 'screenshot', str(shots / (session + '-failure.png'))], capture_output=True, timeout=20)
                result = subprocess.run(['agent-browser', '--session', session, 'snapshot', '-i'], capture_output=True, text=True, timeout=20)
                (shots / (session + '-failure.txt')).write_text(result.stdout + result.stderr)
                for diagnostic in ('errors', 'console'):
                    captured = subprocess.run(['agent-browser', '--session', session, diagnostic, '--json'], capture_output=True, text=True, timeout=20)
                    (shots / (session + '-' + diagnostic + '.json')).write_text(captured.stdout + captured.stderr)
                captured = subprocess.run(['agent-browser', '--session', session, 'eval', '--json', 'JSON.stringify({notices:globalThis.dualProbe?.notices,phase:globalThis.KrillMobile?.store?.connection?.phase,sessions:globalThis.KrillMobile?.store?.sessions?.size,blocked:globalThis.KrillMobile?.app?.transport?.blocked,ready:globalThis.KrillMobile?.app?.transport?.ready})'], capture_output=True, text=True, timeout=20)
                (shots / (session + '-state.json')).write_text(captured.stdout + captured.stderr)
            raise
        finally:
            for session in sessions:
                subprocess.run(['agent-browser', '--session', session, 'close'], capture_output=True, timeout=20)
            for process in reversed(owned):
                stop(process)
            until(lambda: ts_json('serve', 'status', '--json') == before, 30)
            observations['serve_configuration_restored_exactly'] = True
            (shots / 'results.json').write_text(json.dumps(observations, indent=2) + '\n')


if __name__ == '__main__':
    main()

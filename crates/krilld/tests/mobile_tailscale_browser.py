"""Real Chromium QR-photo/paste pairing from HTTPS to a Tailscale daemon.

Requires built krill/krilld/krill-holder and --address set to this Mac's tailnet IP.
Only the HTTPS relay routing is substituted; QR encoding/decoding, direct HTTP,
E2EE, RPC, reconnect and terminal input use the real product implementations.
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
import zlib

from mobile_browser import until


def qr_photo(output, destination):
    """Render the product CLI's own half-block QR as a PNG, without a QR library."""
    plain = re.sub(r'\x1b\[[0-9;]*m', '', output)
    lines = plain.splitlines()
    rows = []
    for line in lines:
        if len(line) != 65 or any(c not in ' █▀▄' for c in line):
            break
        rows.extend([[0 if c in dark else 255 for c in line] for dark in ('█▀', '█▄')])
    assert len(rows) == 66, 'expected the product version-10 QR including quiet zone'
    scale = 8
    pixels = b''.join((b'\0' + bytes(v for v in row for _ in range(scale))) * scale for row in rows)

    def chunk(kind, body):
        return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', binascii.crc32(kind + body))

    destination.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 65 * scale, 66 * scale, 8, 0, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))
    return next(line for line in lines if line.startswith('http://'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--address', required=True)
    parser.add_argument('--screenshots', default='/tmp/krill-tailscale-browser')
    parser.add_argument('--baseline', help='serve client.js from this Git revision to reproduce the bug')
    args = parser.parse_args()
    binaries = Path(os.environ.get('KRILL_MOBILE_TEST_BIN', str(Path(os.environ['CARGO_TARGET_DIR']) / 'debug')))
    tests = Path(__file__).resolve().parent
    assets = tests.parent / 'src/mobile/assets'
    shots = Path(args.screenshots).resolve()
    shots.mkdir(parents=True, exist_ok=True)
    session = f'krill-tailscale-{os.getpid()}'
    fixture = proxy = qr_process = qr_log = None
    with tempfile.TemporaryDirectory(prefix='krill-tailscale-') as scratch:
        scratch = Path(scratch)
        metadata = scratch / 'fixture.json'
        log = (scratch / 'fixture.log').open('w+')
        try:
            fixture = subprocess.Popen(
                ['node', str(tests / 'mobile_pwa.cjs'), str(binaries / 'krilld'), '--keep'],
                env=dict(os.environ, M9_BROWSER_METADATA=str(metadata), M9_BROWSER_ADDRESS='127.0.0.1:0'),
                stdout=log, stderr=log,
            )
            until(lambda: metadata.exists() or fixture.poll() is not None, 90)
            if not metadata.exists():
                log.seek(0)
                raise AssertionError(log.read())
            config = json.loads(metadata.read_text())
            env = dict(os.environ, KRILL_STATE_DIR=config['dir'])

            def cli(*command):
                return subprocess.check_output([str(binaries / 'krill'), *command], env=env, text=True, timeout=25)

            offer = json.loads(cli('mobile', 'pair', '--tailscale', '--address', args.address + ':0', '--json'))['PairingOffer']['url']
            origin = offer.split('/#')[0]
            router = scratch / 'relay.cjs'
            # A real TLS origin is required: HTTP localhost would miss the mixed-content boundary.
            router.write_text(r"""
const https=require('node:https'),net=require('node:net'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const [root,target,ready,fixtures,baseline]=process.argv.slice(2),route=new URL(target),hash='0123456789abcdef';
const cert=new crypto.X509Certificate(fs.readFileSync(path.join(fixtures,'mobile-relay-localhost.der'))).toString();
const key=crypto.createPrivateKey({key:fs.readFileSync(path.join(fixtures,'mobile-relay-localhost-key.der')),format:'der',type:'pkcs8'}).export({format:'pem',type:'pkcs8'});
const server=https.createServer({cert,key},(req,res)=>{
 const u=new URL(req.url,'https://localhost');res.setHeader('Cache-Control','no-store');
 if(u.pathname.startsWith('/r/')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({relay:true,hash}));return;}
 const file=u.pathname.replace(/^\/m\/[0-9a-f]{16}\//,'').replace(/^\//,'')||'index.html',resolved=path.resolve(root,file);
 if(!resolved.startsWith(root+path.sep)){res.writeHead(403);res.end();return;}
 try{const data=file==='client.js'&&baseline?fs.readFileSync(baseline):fs.readFileSync(resolved);res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.html')?'text/html':file.endsWith('.png')?'image/png':file.endsWith('.svg')?'image/svg+xml':'application/json');res.end(data);}catch{res.writeHead(404);res.end();}
});
server.on('upgrade',(req,socket,head)=>{
 const upstream=net.connect(Number(route.port),route.hostname,()=>{
  const headers={...req.headers,host:route.host,origin:route.origin};
  upstream.write('GET /ws HTTP/1.1\r\n'+Object.entries(headers).map(([k,v])=>k+': '+v).join('\r\n')+'\r\n\r\n');
  if(head.length)upstream.write(head);socket.pipe(upstream);upstream.pipe(socket);
 });upstream.on('error',()=>socket.destroy());socket.on('error',()=>upstream.destroy());socket.on('close',()=>upstream.destroy());
});
server.listen(0,'127.0.0.1',()=>fs.writeFileSync(ready,String(server.address().port)));
""")
            baseline = scratch / 'baseline.js'
            if args.baseline:
                baseline.write_bytes(subprocess.check_output(['git', 'show', args.baseline + ':crates/krilld/src/mobile/assets/client.js']))
            ready = scratch / 'port'
            proxy = subprocess.Popen(['node', str(router), str(assets), origin, str(ready), str(tests / 'fixtures'), str(baseline) if args.baseline else ''])
            until(ready.exists)
            base = f'https://localhost:{ready.read_text()}/m/0123456789abcdef/'

            def browser(*command):
                result = subprocess.run(['agent-browser', '--session', session, *command], text=True, capture_output=True, timeout=45)
                if result.returncode:
                    subprocess.run(['agent-browser', '--session', session, 'screenshot', str(shots / 'failure.png')], capture_output=True, timeout=15)
                    detail = subprocess.run(['agent-browser', '--session', session, 'snapshot', '-i'], text=True, capture_output=True, timeout=15)
                    raise AssertionError(result.stdout + result.stderr + detail.stdout)
                return result.stdout

            def evaluate(script):
                return json.loads(browser('eval', '--json', script))['data']['result']

            def wait(script):
                browser('wait', '--fn', script)

            def click(name):
                browser('find', 'role', 'button', 'click', '--name', name, '--exact')

            # Headed Chrome, real QR photo decoding (not a stubbed BarcodeDetector).
            browser('--headed', '--ignore-https-errors', 'open', base + '#' + offer.split('#')[1] + '&r=' + 'a' * 32)
            browser('set', 'viewport', '393', '852')
            wait('KrillMobile.store.connection.phase==="ready"')
            assert evaluate('location.protocol') == 'https:'
            evaluate("localStorage.setItem('krill-mobile-ui-v1',JSON.stringify({language:'en',installOffered:true}));true")
            browser('reload')
            wait('KrillMobile.store.connection.phase==="ready"')
            evaluate("KrillMobile.app.navigate('/');true")
            wait('!!document.querySelector(".host-card")')
            saved = evaluate('localStorage.getItem("krill-mobile-hosts-v1")')
            browser('screenshot', str(shots / 'relay-paired.png'))

            # Take the QR from the real CLI, then give its PNG to the same scanner as the phone.
            qr_log = (scratch / 'qr.txt').open('w+')
            qr_process = subprocess.Popen([str(binaries / 'krill'), 'mobile', 'pair', '--tailscale', '--address', origin.split('://')[1]], env=env, stdout=qr_log, stderr=qr_log)
            until(lambda: 'Waiting for pairing' in (scratch / 'qr.txt').read_text())
            photo = shots / 'tailscale-qr.png'
            link = qr_photo((scratch / 'qr.txt').read_text(), photo)
            click('Pair Desktop')
            wait('!!document.querySelector("dialog.scanner")')
            browser('upload', 'dialog.scanner input[type=file]', str(photo))
            if args.baseline:
                wait('document.querySelector(".scanner-hint")?.textContent.includes("This direct code cannot be opened here")')
                browser('screenshot', str(shots / 'baseline-rejected.png'))
                raise AssertionError('reproduced: released scanner rejects a real Tailscale QR')
            wait('location.protocol==="http:" && KrillMobile.store.connection.phase==="ready"')
            assert evaluate('location.origin') == origin
            assert evaluate('isSecureContext') is False
            assert evaluate('typeof crypto.subtle') == 'undefined'
            assert evaluate('KrillMobile.store.sessions.size') >= 1
            assert 'pair=' not in evaluate('location.href')
            qr_process.wait(timeout=25)
            assert qr_process.returncode == 0
            browser('screenshot', str(shots / 'tailscale-paired.png'))
            browser('reload')
            wait('KrillMobile.store.connection.phase==="ready"')

            sid = config['session']
            evaluate(f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({sid}).groupKey)+"/s/{sid}");true')
            wait('!!KrillMobile.activeTerminal && document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("READY")')
            # Drive the visible input and verify the real PTY received exactly one command.
            evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
            browser('fill', '.command-input', 'tailscale-qr-real-browser')
            evaluate('document.querySelector(".buffered-command").requestSubmit();true')
            until(lambda: 'got:tailscale-qr-real-browser' in cli('read', str(sid)))
            assert cli('read', str(sid)).count('got:tailscale-qr-real-browser') == 1
            browser('screenshot', str(shots / 'tailscale-terminal.png'))

            # Visiting the HTTPS app again must recover its original relay pairing unchanged.
            browser('open', base)
            wait('KrillMobile.store.connection.phase==="ready"')
            evaluate("KrillMobile.app.navigate('/');true")
            wait('!!document.querySelector(".host-card")')
            assert evaluate('localStorage.getItem("krill-mobile-hosts-v1")') == saved
            browser('screenshot', str(shots / 'relay-preserved.png'))

            # Paste uses the same adoption/navigation boundary with a fresh one-use ticket.
            link = json.loads(cli('mobile', 'pair', '--tailscale', '--address', origin.split('://')[1], '--json'))['PairingOffer']['url']
            click('Pair Desktop')
            click('Paste link instead')
            browser('fill', 'input[type=url]', link)
            click('Continue')
            wait('location.protocol==="http:" && KrillMobile.store.connection.phase==="ready"')
            assert evaluate('location.origin') == origin
            browser('screenshot', str(shots / 'tailscale-paste-paired.png'))
            print('PASS real HTTPS → Tailscale QR photo + paste, insecure-context E2EE, reload, PTY input once, saved relay reconnect')
        finally:
            subprocess.run(['agent-browser', '--session', session, 'close'], capture_output=True, timeout=20)
            for process in (qr_process, proxy, fixture):
                if process and process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
            if qr_log:
                qr_log.close()
            log.close()


if __name__ == '__main__':
    main()

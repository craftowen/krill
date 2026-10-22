"""Real Chromium LAN QA with isolated holder/daemon, mock agents and pairing recovery."""
import argparse
import base64
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import time


def until(check, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise AssertionError('browser fixture condition timed out')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--address', required=True)
    parser.add_argument('--screenshots', default='/tmp/mk-mobile/final/chromium')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    target = Path(os.environ.get('CARGO_TARGET_DIR', root / 'target'))
    shots = Path(args.screenshots).resolve()
    shots.mkdir(parents=True, exist_ok=True)
    session = f'mobile-shell-{os.getpid()}'
    with tempfile.TemporaryDirectory(prefix='mobile-shell-') as scratch:
        metadata = Path(scratch) / 'fixture.json'
        env = dict(os.environ, M9_UI_QA='1', M9_BROWSER_ADDRESS=args.address, M9_BROWSER_METADATA=str(metadata))
        with (Path(scratch) / 'fixture.log').open('w+') as log:
            fixture = subprocess.Popen(['node', str(Path(__file__).with_name('mobile_pwa.cjs')), str(target / 'debug/krilld'), '--keep'], env=env, stdout=log, stderr=log)
            try:
                until(metadata.exists, 90)
                cfg = json.loads(metadata.read_text())
                def browser(*command):
                    r = subprocess.run(['agent-browser', '--session', session, *command], capture_output=True, text=True, timeout=45)
                    assert r.returncode == 0, r.stdout + r.stderr
                    return r.stdout
                def evaluate(script):
                    return json.loads(browser('eval', '--json', script))['data']['result']
                def wait(script):
                    browser('wait', '--fn', script)
                def cli(*command):
                    return subprocess.check_output([str(target/'debug/krill'), *command], env=dict(os.environ, KRILL_STATE_DIR=cfg['dir']), text=True)
                def navigate(route):
                    evaluate(f'KrillMobile.app.navigate({json.dumps(route)});true')
                def shot(name):
                    browser('screenshot', str(shots / f'{name}.png'))
                fault = Path(scratch) / 'interrupt.js'
                fault.write_text("""
const NativeWebSocket=WebSocket;globalThis.recoverySockets=[];
globalThis.WebSocket=class extends NativeWebSocket {
 constructor(...args){super(...args);recoverySockets.push(this);let received=0;
 this.addEventListener('message',e=>{if((++received===3&&!sessionStorage.getItem('interrupted'))||this.dropReply){this.dropReply=false;sessionStorage.setItem('interrupted','yes');e.stopImmediatePropagation();this.close();}});}
 send(data){if(this.dropAfterSend){this.dropAfterSend=false;this.dropReply=true;}return super.send(data);}
};
""")
                browser('--init-script', str(fault), 'open', cfg['url'])
                browser('set','viewport','390','844')
                browser('find','role','button','click','--name','Pair','--exact')
                wait('KrillMobile.store.connection.phase==="ready" && recoverySockets.length>=2')
                assert evaluate('sessionStorage.getItem("interrupted")') == 'yes'
                assert len(json.loads((Path(cfg['dir'])/'mobile-devices.json').read_text())) == 1
                assert evaluate('({secure:isSecureContext,subtle:typeof crypto.subtle,paired:!!localStorage.getItem("minikrill-mobile-v2"),hash:location.hash})') == {'secure':False,'subtle':'undefined','paired':True,'hash':'#/'}
                assert evaluate('document.querySelector(".host-card").getBoundingClientRect().bottom<innerHeight')
                shot('home')
                browser('click','.host-main')
                wait('document.querySelectorAll(".workspace-row").length===4')
                assert evaluate('KrillMobile.store.snapshot.repositories.length') == 2
                assert evaluate('document.querySelector(".workspace-row").getBoundingClientRect().bottom<innerHeight')
                shot('workspaces')
                browser('find','role','button','click','--name','Search','--exact')
                browser('fill','input[type=search]','feature-worktree')
                assert evaluate('document.querySelectorAll(".workspace-row").length') == 1
                browser('fill','input[type=search]','')
                evaluate(f'KrillMobile.app.navigate(KrillMobile.store.sessions.has({cfg["session"]})?"/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({cfg["session"]}).groupKey)+"/s/{cfg["session"]}":"/");true')
                wait('document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("READY")')
                shot('session-direct')
                browser('find','role','button','click','--name','Terminal actions','--exact')
                shot('action-sheet')
                browser('find','role','button','click','--name','Send prompt','--exact')
                browser('fill','#prompt','mobile-reconnect-once')
                evaluate('recoverySockets[1].dropAfterSend=true;document.getElementById("composer").requestSubmit();true')
                wait('recoverySockets.length>=4&&KrillMobile.store.connection.phase==="ready"')
                assert cli('read',str(cfg['session'])).count('got:mobile-reconnect-once') == 1
                assert evaluate('document.getElementById("prompt").value') == 'mobile-reconnect-once'
                assert 'not resent' in evaluate('document.getElementById("notice").textContent')
                browser('find','role','button','click','--name','Cancel','--exact')
                browser('find','role','button','click','--name','Terminal actions','--exact')
                browser('find','role','button','click','--name','Approve (Yes)','--exact')
                until(lambda:'got:y' in cli('read',str(cfg['session'])))
                evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
                browser('fill','.command-input','한글 조합 테스트')
                evaluate('document.querySelector(".command-input").dispatchEvent(new CompositionEvent("compositionstart"));document.querySelector(".buffered-command").requestSubmit();true')
                assert 'got:한글 조합 테스트' not in cli('read',str(cfg['session']))
                evaluate('document.querySelector(".command-input").dispatchEvent(new CompositionEvent("compositionend"));document.querySelector(".buffered-command").requestSubmit();true')
                until(lambda:'got:한글 조합 테스트' in cli('read',str(cfg['session'])))
                shot('session-buffered')
                photo = Path(scratch)/'photo.png'
                photo.write_bytes(base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a9sAAAAASUVORK5CYII='))
                browser('upload','.photo-picker',str(photo))
                wait('document.getElementById("notice").textContent.includes("Photo attached")')
                uploads=list((Path(cfg['dir'])/'paste/krill-paste').glob('mobile-*.jpg'))
                assert len(uploads)==1 and stat.S_IMODE(uploads[0].stat().st_mode)==0o600
                original_route=evaluate('location.hash.slice(1)')
                browser('find','role','button','click','--name','Create Tab','--exact')
                browser('find','role','button','click','--name','Terminal','--exact')
                wait('KrillMobile.store.sessions.size===6')
                wait(f'location.hash!=="#{original_route}"')
                created=evaluate('Number(location.hash.split("/").at(-1))')
                assert created!=cfg['session']
                shot('created-terminal')
                (shots/'sessions-created.txt').write_text(cli('ls'))
                browser('find','role','button','click','--name','Terminal actions','--exact')
                browser('find','role','button','click','--name','Close terminal','--exact')
                shot('close-confirm')
                browser('find','role','button','click','--name','Close terminal','--exact')
                wait('KrillMobile.store.sessions.size===5')
                (shots/'sessions-closed.txt').write_text(cli('ls'))
                navigate('/orchestration')
                browser('find','role','button','click','--name','Approve','--exact')
                browser('fill','textarea[placeholder="Reply to worker"]','모바일 응답')
                assert evaluate('new Promise(r=>setTimeout(()=>r(document.querySelector("#orchestration textarea")?.value),3100))')=='모바일 응답'
                shot('orchestration')
                browser('find','role','button','click','--name','Send reply','--exact')
                wait('!document.querySelector("#orchestration textarea")')
                navigate('/accounts')
                wait('Array.from(document.querySelectorAll(".account-card h2")).some(e=>e.textContent==="Mobile fixture")')
                account='Array.from(document.querySelectorAll(".account-card")).find(e=>e.querySelector("h2").textContent==="Mobile fixture")'
                assert evaluate(f'Array.from(({account}).querySelectorAll("progress")).map(e=>e.value)')==[23,47]
                evaluate(f'({account}).querySelector("button").click();true')
                wait(f'!!({account}).querySelector(".active-account")')
                shot('accounts')
                navigate(f'/git/{cfg["session"]}')
                wait('!!Array.from(document.querySelectorAll(".file-row")).find(e=>e.textContent==="example.txt")')
                browser('find','role','button','click','--name','example.txt','--exact')
                wait('document.getElementById("git-patch").textContent.includes("+after")')
                shot('git')
                navigate('/settings')
                shot('settings')
                navigate('/settings/terminal')
                browser('select','select[aria-label="Font size"]','1.25')
                browser('find','role','checkbox','click','--name','Esc','--exact')
                assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-ui-v1")).visibleKeyIds.includes("escape")') is False
                shot('terminal-settings')
                navigate(original_route)
                wait('document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("READY")')
                assert evaluate('KrillMobile.activeTerminal.getInputMode()')=='buffered'
                assert not evaluate('!!document.querySelector("[data-key-id=escape]")')
                for width,height,label in [(320,844,'narrow'),(844,390,'landscape')]:
                    browser('set','viewport',str(width),str(height))
                    assert not evaluate('document.documentElement.scrollWidth>innerWidth')
                    assert evaluate('document.querySelector(".command-dock").getBoundingClientRect().bottom<=innerHeight+1')
                    shot(label)
                browser('reload')
                wait('KrillMobile.store.connection.phase==="ready"')
                assert evaluate('KrillMobile.activeTerminal.getInputMode()')=='buffered'
                assert not browser('errors').strip(),browser('errors')
                # A pre-extension daemon advertises no create/close support.
                evaluate('KrillMobile.store.snapshot.capabilities={};KrillMobile.app.navigate("/host/local");true')
                navigate(original_route)
                wait('!!document.querySelector(".session-tabs")')
                assert not evaluate('!!document.querySelector(".tab-add")')
                browser('find','role','button','click','--name','Terminal actions','--exact')
                assert not evaluate('Array.from(document.querySelectorAll(".sheet-action")).some(e=>e.textContent==="Close terminal")')
                print('Chromium LAN QA passed: pairing recovery, no mutation replay, grouped navigation, IME, photo, lifecycle, orchestration, accounts, Git, settings, narrow/landscape and reload')
            finally:
                subprocess.run(['agent-browser','--session',session,'close'],capture_output=True,timeout=15)
                fixture.terminate()
                try: fixture.wait(timeout=20)
                except subprocess.TimeoutExpired: fixture.kill();fixture.wait()
                if fixture.returncode not in (0,-15):
                    log.seek(0);print(log.read())


if __name__=='__main__':
    main()

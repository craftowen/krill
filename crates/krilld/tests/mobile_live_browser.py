"""MK-126 real Chromium + macOS GUI/memory QA, using isolated PTYs and state.

Build workspace bins first. agent-browser and native macOS GUI access are required.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
from mobile_browser import until


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--address', required=True)
    parser.add_argument('--browser-only',action='store_true',help='Run live PWA checks without launching or measuring the native app')
    parser.add_argument('--evidence', default='/tmp/m9-live-qa')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    target = Path(os.environ.get('CARGO_TARGET_DIR', root / 'target'))
    evidence = Path(args.evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    session = f'm9-live-{os.getpid()}'
    second_session = session + '-second'
    with tempfile.TemporaryDirectory(prefix='m9live-') as scratch:
        metadata = Path(scratch) / 'fixture.json'
        env = dict(os.environ, M9_BROWSER_ADDRESS=args.address, M9_BROWSER_METADATA=str(metadata), M9_LIVE_QA='1')
        app = None
        with (evidence / 'fixture.log').open('w+') as log:
            fixture = subprocess.Popen(['node', str(Path(__file__).with_name('mobile_pwa.cjs')), str(target / 'debug/krilld'), '--keep'], env=env, stdout=log, stderr=log)
            try:
                until(metadata.exists, seconds=90)
                config = json.loads(metadata.read_text())
                def browser(*args):
                    result = subprocess.run(['agent-browser', '--session', session, *args], text=True, capture_output=True, timeout=45)
                    assert result.returncode == 0, result.stdout + result.stderr
                    return result.stdout
                def evaluate(script):
                    return json.loads(browser('eval', '--json', script))['data']['result']
                def wait(script):
                    try: browser('wait', '--fn', script)
                    except Exception:
                        print('BROWSER STATE',evaluate('({notice:document.getElementById("notice")?.textContent,connection:globalThis.KrillMobile?.store.connection,terminal:document.querySelector(".terminal-status")?.textContent,screen:document.querySelector(".terminal-grid")?.getAttribute("aria-label")})'),flush=True)
                        print('PTY',cli('read',str(config['terminalSession'])),flush=True)
                        raise
                def cli(*args):
                    return subprocess.check_output([str(target/'debug/krill'), *args], env=dict(os.environ, KRILL_STATE_DIR=config['dir']), text=True, timeout=30)
                def footprint(pid, label):
                    result = subprocess.run(['/usr/bin/footprint','-p',str(pid),'-f','bytes'], text=True,capture_output=True,timeout=30)
                    (evidence/f'footprint-{label}.txt').write_text(result.stdout + result.stderr)
                    match = re.search(r'Footprint: (\d+) B',result.stdout)
                    assert match, result.stdout + result.stderr
                    return int(match[1])
                def size():
                    return evaluate('(()=>{const m=document.querySelector(".terminal-status").textContent.match(/(\\d+) × (\\d+)/);return {cols:Number(m[1]),rows:Number(m[2])};})()')
                expected_ptys = 9 if args.browser_only else 10
                if not args.browser_only:
                    # The desktop is part of the baseline: nine fixture PTYs + its default pane = ten.
                    (Path(config['dir'])/'onboarding.done').touch()
                    app_log=(evidence/'app.log').open('w')
                    app=subprocess.Popen([str(target/'debug/krill-app')],env=dict(os.environ,KRILL_STATE_DIR=config['dir'],HOME=str(Path(config['dir'])/'home'),KRILL_LOG_DIR=str(evidence/'process-logs')),cwd=config['repo'],stdout=app_log,stderr=app_log)
                    time.sleep(12)
                    assert app.poll() is None, f'native app exited: {app.returncode}; see {evidence}/process-logs'
                    assert len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]) == expected_ptys
                baseline = footprint(config['daemon'],'daemon-baseline')
                init=Path(scratch)/'sockets.js'
                init.write_text('const Native=WebSocket;globalThis.qaSockets=[];globalThis.WebSocket=class extends Native{constructor(...a){super(...a);qaSockets.push(this);}};')
                browser('--init-script',str(init),'open',config['url'])
                browser('set','viewport','390','844')
                browser('find','role','button','click','--name','Pair','--exact')
                wait('KrillMobile.store.connection.phase==="ready"')
                terminal = config['terminalSession']
                assert len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]) == expected_ptys
                # Select via its actual card, then render the real screen stream.
                evaluate(f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({terminal}).groupKey)+"/s/{terminal}");true')
                wait('document.querySelector(".terminal-grid").getAttribute("aria-label").includes("LIVE RED")')
                original = size()
                assert evaluate('document.querySelector(".terminal-grid").getAttribute("aria-label").includes("한")')
                assert evaluate('document.querySelector(".terminal-grid").getAttribute("aria-label").includes("é")')
                browser('screenshot',str(evidence/'live-grid.png'))
                for key_id, encoded in [('escape','1b'),('tab','09'),('enter','0d'),('shiftTab','1b5b5a'),('space','20'),('backspace','7f'),('delete','1b5b337e'),('arrowUp','1b5b41'),('arrowDown','1b5b42'),('arrowLeft','1b5b44'),('arrowRight','1b5b43'),('ctrlC','03'),('ctrlD','04'),('ctrlL','0c'),('ctrlZ','1a'),('ctrlR','12'),('ctrlA','01'),('ctrlE','05'),('ctrlW','17'),('ctrlU','15')]:
                    selector = f'.keybar [data-key-id="{key_id}"]'
                    evaluate(f'document.querySelector({json.dumps(selector)}).click();true')
                    wait(f'document.querySelector(".terminal-grid").getAttribute("aria-label").includes("KEYS:{encoded}")')
                evaluate('KrillMobile.activeTerminal.fitToPhone();true')
                wait('document.querySelector(".display-mode").getAttribute("aria-pressed")==="true"')
                fitted = size()
                assert fitted['cols'] == 44 or 10 <= fitted['cols'] <= 48
                browser('screenshot',str(evidence/'fit-phone.png'))
                evaluate('KrillMobile.activeTerminal.fitToPhone();true')
                wait(f'document.querySelector(".terminal-status").textContent.startsWith("{original["cols"]} ×")')
                # Retake size ownership, then close the stream without a courtesy resize.
                evaluate('KrillMobile.activeTerminal.fitToPhone();true')
                wait('document.querySelector(".display-mode").getAttribute("aria-pressed")==="true"')
                evaluate('qaSockets.at(-1).close();true')
                wait('document.querySelector(".terminal-status").textContent.includes("Disconnected")')
                evaluate('KrillMobile.app.navigate("/host/local");true')
                evaluate(f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({terminal}).groupKey)+"/s/{terminal}");true')
                wait(f'document.querySelector(".terminal-status")?.textContent.startsWith("{original["cols"]} ×")')
                # Lazy hot paging followed by zstd cold replay; only 1000 rows stay in the browser.
                for _ in range(35):
                    evaluate('KrillMobile.activeTerminal.loadHistory();true')
                    wait('!document.querySelector(".history-status").textContent.startsWith("Loading")')
                    if evaluate('document.querySelector(".history-status").textContent.startsWith("Cold")'):
                        break
                assert evaluate('document.querySelector(".history-status").textContent.startsWith("Cold")')
                assert evaluate('document.querySelector(".terminal-history").textContent.split("\\n").length<=1001')
                assert evaluate('document.querySelector(".terminal-history").textContent.includes("history 한글")')
                browser('screenshot',str(evidence/'cold-history.png'))
                # The URL used by ntfy/Telegram/Discord reopens exactly this session.
                browser('open',config['base']+f'/?session={terminal}')
                wait(f'location.hash.endsWith("/s/{terminal}") && document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("KEYS:15")')
                assert evaluate('new URLSearchParams(location.search).get("session")') is None
                assert not evaluate('document.documentElement.scrollWidth>innerWidth')
                browser('screenshot',str(evidence/'deep-link.png'))
                assert not browser('errors').strip()
                # Pair a second device and keep its workflow + screen streams live too.
                second_offer = json.loads(cli('mobile','pair','--address',args.address,'--json'))['PairingOffer']['url']
                def second(*args):
                    result = subprocess.run(['agent-browser','--session',second_session,*args],text=True,capture_output=True,timeout=45)
                    assert result.returncode == 0, result.stdout + result.stderr
                    return result.stdout
                second('open',second_offer)
                second('set','viewport','390','844')
                second('find','role','button','click','--name','Pair','--exact')
                second('wait','--fn','KrillMobile.store.connection.phase==="ready"')
                second('eval',f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({terminal}).groupKey)+"/s/{terminal}");true')
                second('wait','--fn','document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("KEYS:15")')
                two_mobile = footprint(config['daemon'],'daemon-two-mobile')
                measurements={'daemon_before_live':baseline,'daemon_with_two_mobile':two_mobile,'mobile_increase':two_mobile-baseline,'daemon_with_live':footprint(config['daemon'],'daemon-two-mobile'),'app_ten_ptys':footprint(app.pid,'app') if app else None,'pty_count':len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]),'daemon_pid':config['daemon'],'app_pid':app.pid if app else None,'original':original,'fit':fitted,'terminal':terminal}
                measurements['daemon_peak']=int(re.search(r'phys_footprint_peak: (\d+) B',(evidence/'footprint-daemon-two-mobile.txt').read_text()).group(1))
                (evidence/'measurements.json').write_text(json.dumps(measurements,indent=2))
                # Preserve isolated fixture long enough for the caller to capture Mobile Settings.
                if app and os.environ.get('M9_GUI_HOLD'):
                    (evidence/'gui-ready.json').write_text(json.dumps(config|{'app_pid':app.pid if app else None}))
                    until(lambda:(evidence/'gui-done').exists(),seconds=600)
                assert measurements['pty_count'] == expected_ptys, measurements
                assert measurements['mobile_increase'] <= 5*1024*1024, measurements
                assert measurements['daemon_peak'] <= 60*1024*1024, measurements
                assert measurements['daemon_with_live'] <= 60*1024*1024, measurements
                if app: assert measurements['app_ten_ptys'] <= 120*1024*1024, measurements
                print(json.dumps(measurements,indent=2))
                print(f'PASS: live snapshot/deltas, ANSI/wide/combining text, 20 keys, Fit release/disconnect restoration, hot+cold history, deep link, {expected_ptys} PTYs; native memory {"not requested" if args.browser_only else "verified"}')
            finally:
                if app:
                    app.kill();app.wait(timeout=10)
                for browser_session in [session, second_session]:
                    subprocess.run(['agent-browser','--session',browser_session,'close'],capture_output=True,timeout=20)
                fixture.terminate()
                try:fixture.wait(timeout=25)
                except subprocess.TimeoutExpired:fixture.kill();fixture.wait()
                if fixture.returncode not in (0,-15):
                    log.seek(0);print(log.read())

if __name__=='__main__':
    main()

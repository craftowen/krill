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
    parser.add_argument('--evidence', default='/tmp/m9-live-qa')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    evidence = Path(args.evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    session = f'm9-live-{os.getpid()}'
    second_session = session + '-second'
    with tempfile.TemporaryDirectory(prefix='m9live-') as scratch:
        metadata = Path(scratch) / 'fixture.json'
        env = dict(os.environ, M9_BROWSER_ADDRESS=args.address, M9_BROWSER_METADATA=str(metadata), M9_LIVE_QA='1')
        app = None
        with (evidence / 'fixture.log').open('w+') as log:
            fixture = subprocess.Popen(['node', str(Path(__file__).with_name('mobile_pwa.cjs')), str(root / 'target/debug/krilld'), '--keep'], env=env, stdout=log, stderr=log)
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
                        print('BROWSER STATE',evaluate('({notice:document.getElementById("notice")?.textContent,ready:globalThis.liveTerminal?.ready,pending:globalThis.liveTerminal?.pendingInputs.size,screen:document.getElementById("terminal-grid")?.getAttribute("aria-label")})'),flush=True)
                        print('PTY',cli('read',str(config['terminalSession'])),flush=True)
                        raise
                def cli(*args):
                    return subprocess.check_output([str(root/'target/debug/krill'), *args], env=dict(os.environ, KRILL_STATE_DIR=config['dir']), text=True, timeout=30)
                def footprint(pid, label):
                    result = subprocess.run(['/usr/bin/footprint','-p',str(pid),'-f','bytes'], text=True,capture_output=True,timeout=30)
                    (evidence/f'footprint-{label}.txt').write_text(result.stdout + result.stderr)
                    match = re.search(r'Footprint: (\d+) B',result.stdout)
                    assert match, result.stdout + result.stderr
                    return int(match[1])
                def size():
                    return evaluate('({cols:liveTerminal.model.cols,rows:liveTerminal.model.lines})')
                # The desktop is part of the baseline: nine fixture PTYs + its default pane = ten.
                (Path(config['dir'])/'onboarding.done').touch()
                app_log=(evidence/'app.log').open('w')
                app=subprocess.Popen([str(root/'target/debug/krill-app')],env=dict(os.environ,KRILL_STATE_DIR=config['dir'],HOME=str(Path(config['dir'])/'home')),cwd=config['repo'],stdout=app_log,stderr=app_log)
                time.sleep(12)
                assert app.poll() is None, 'native app exited'
                assert len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]) == 10
                baseline = footprint(config['daemon'],'daemon-baseline')
                browser('open',config['url'])
                browser('set','viewport','390','844')
                wait('document.getElementById("connection").textContent.includes("connected")')
                terminal = config['terminalSession']
                assert len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]) == 10
                # Select via its actual card, then render the real screen stream.
                browser('click',f'[data-session="{terminal}"] button.primary')
                wait('globalThis.liveTerminal?.ready && document.getElementById("terminal-grid").getAttribute("aria-label").includes("LIVE RED")')
                original = size()
                assert evaluate('liveTerminal.model.rows.flat().some(c=>c.ch==="한"&&(c.flags&8))')
                assert evaluate('liveTerminal.model.rows.flat().some(c=>c.zero.includes("́"))')
                browser('screenshot',str(evidence/'live-grid.png'))
                for label, encoded in [('Esc','1b'),('Tab','09'),('⇧Tab','1b5b5a'),('Ctrl+C','03'),('Ctrl+D','04'),('↑','1b5b41'),('↓','1b5b42'),('←','1b5b44'),('→','1b5b43'),('Enter','0d')]:
                    evaluate(f'Array.from(document.querySelectorAll("#keybar button")).find(b=>b.textContent==={json.dumps(label)}).click();true')
                    wait(f'document.getElementById("terminal-grid").getAttribute("aria-label").includes("KEYS:{encoded}")')
                browser('check','#fit-phone')
                wait('liveTerminal.model.cols<60')
                fitted = size()
                assert fitted['cols'] == 44 or 10 <= fitted['cols'] <= 48
                browser('screenshot',str(evidence/'fit-phone.png'))
                browser('uncheck','#fit-phone')
                wait(f'liveTerminal.model.cols==={original["cols"]}')
                # Retake size ownership, then close the stream without a courtesy resize.
                browser('check','#fit-phone')
                wait('liveTerminal.model.cols<60')
                evaluate('liveTerminal.socket.close();true')
                wait('!liveTerminal.ready')
                browser('click',f'[data-session="{terminal}"] button.primary')
                wait(f'liveTerminal.ready && liveTerminal.model.cols==={original["cols"]}')
                # Lazy hot paging followed by zstd cold replay; only 1000 rows stay in the browser.
                for _ in range(35):
                    evaluate('liveTerminal.loadHistory();true')
                    wait('!liveTerminal.historyPending')
                    if evaluate('document.getElementById("history-status").textContent.startsWith("Cold")'):
                        break
                assert evaluate('document.getElementById("history-status").textContent.startsWith("Cold")')
                assert evaluate('liveTerminal.historyRows.length<=1000')
                assert evaluate('document.getElementById("terminal-history").textContent.includes("history 한글")')
                browser('screenshot',str(evidence/'cold-history.png'))
                # The URL used by ntfy/Telegram/Discord reopens exactly this session.
                browser('open',config['base']+f'/?session={terminal}')
                wait(f'liveTerminal?.ready && liveTerminal.session==={terminal}')
                assert evaluate('new URLSearchParams(location.search).get("session")') == str(terminal)
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
                second('wait','--fn','document.getElementById("connection").textContent.includes("connected")')
                second('click',f'[data-session="{terminal}"] button.primary')
                second('wait','--fn','liveTerminal?.ready')
                two_mobile = footprint(config['daemon'],'daemon-two-mobile')
                measurements={'daemon_before_live':baseline,'daemon_with_two_mobile':two_mobile,'mobile_increase':two_mobile-baseline,'daemon_with_live':footprint(config['daemon'],'daemon-two-mobile'),'app_ten_ptys':footprint(app.pid,'app'),'pty_count':len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]),'daemon_pid':config['daemon'],'app_pid':app.pid,'original':original,'fit':fitted,'terminal':terminal}
                measurements['daemon_peak']=int(re.search(r'phys_footprint_peak: (\d+) B',(evidence/'footprint-daemon-two-mobile.txt').read_text()).group(1))
                (evidence/'measurements.json').write_text(json.dumps(measurements,indent=2))
                # Preserve isolated fixture long enough for the caller to capture Mobile Settings.
                if os.environ.get('M9_GUI_HOLD'):
                    (evidence/'gui-ready.json').write_text(json.dumps(config|{'app_pid':app.pid}))
                    until(lambda:(evidence/'gui-done').exists(),seconds=600)
                assert measurements['pty_count'] == 10, measurements
                assert measurements['mobile_increase'] <= 5*1024*1024, measurements
                assert measurements['daemon_peak'] <= 60*1024*1024, measurements
                assert measurements['daemon_with_live'] <= 60*1024*1024, measurements
                assert measurements['app_ten_ptys'] <= 120*1024*1024, measurements
                print(json.dumps(measurements,indent=2))
                print('PASS: live snapshot/deltas, ANSI/wide/combining cells, ten keys, Fit release/disconnect restoration, hot+cold history, deep link, ten PTYs, native app memory')
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

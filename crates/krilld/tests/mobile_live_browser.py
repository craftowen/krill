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


def verify_touch_scroll_ux(config, cli, evidence):
    """Playwright mobile touch (390x844, has_touch=True) E2E scroll & zero-error gate."""
    from playwright.sync_api import sync_playwright

    stream_sid = int(
        cli(
            'new', '--cwd', config['repo'], '--', '/bin/sh', '-c',
            "i=1; while [ $i -le 1200 ]; do printf 'LINE_%04d: The quick brown fox jumps over the lazy dog 0123456789\\n' $i; i=$((i+1)); done; "
            "echo '=== STREAM_READY ==='; "
            "while true; do i=$((i+1)); printf 'LIVE_%04d: continuous live output while scrolled up\\n' $i; sleep 0.2; done",
        ).strip()
    )
    try:
        until(lambda: 'STREAM_READY' in cli('read', str(stream_sid)), seconds=20)
        console_errors = []
        page_errors = []
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            context = browser.new_context(
                viewport={'width': 390, 'height': 844},
                device_scale_factor=3,
                is_mobile=True,
                has_touch=True,
                user_agent='Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1',
            )
            context.add_init_script('localStorage.setItem("krill-mobile-error-reporting-v1","");')
            page = context.new_page()
            page.on('console', lambda m: console_errors.append(m.text) if m.type == 'error' else None)
            page.on('pageerror', lambda e: page_errors.append(str(e)))
            touch_offer = json.loads(cli('mobile', 'pair', '--address', config['base'].split('://')[1], '--json'))['PairingOffer']['url']
            page.goto(touch_offer, wait_until='domcontentloaded')
            page.wait_for_function('() => globalThis.KrillMobile?.store?.connection?.phase === "ready"', timeout=30000)
            page.evaluate('() => { KrillMobile.install?.dismiss?.(); document.getElementById("action-sheet")?.close?.(); globalThis._unhandled = []; globalThis.addEventListener("unhandledrejection", e => globalThis._unhandled.push(String(e.reason?.stack || e.reason))); globalThis._notices = []; const n = document.getElementById("notice"); new MutationObserver(() => { if (!n.hidden && n.textContent) globalThis._notices.push(n.textContent); }).observe(n, {subtree:true, childList:true, characterData:true, attributes:true}); }')

            def open_session(sid):
                page.wait_for_function(f'() => KrillMobile.store.sessions.has({sid})', timeout=15000)
                page.evaluate(f'() => {{ const s = KrillMobile.store.sessions.get({sid}); KrillMobile.app.navigate("/host/local/group/" + encodeURIComponent(s.groupKey) + "/s/{sid}"); }}')
                page.wait_for_function('() => document.querySelector(".terminal-mount")?.dataset.phase === "ready"', timeout=15000)

            def snap():
                return page.evaluate('''() => {
                    const m = document.querySelector('.terminal-mount');
                    const p = document.querySelector('.terminal-scroll');
                    const h = document.querySelector('.terminal-history');
                    const c = document.querySelector('.terminal-grid');
                    const l = document.querySelector('.terminal-latest');
                    const r = p?.getBoundingClientRect();
                    let firstLine = null;
                    if (p && h && !h.hidden) {
                        const pr = p.getBoundingClientRect();
                        for (const ch of h.children) {
                            const cr = ch.getBoundingClientRect();
                            if (cr.bottom > pr.top + 2) {
                                const txt = (ch.textContent || '').trim();
                                const m1 = txt.match(/(?:LINE_|LIVE_|^)(\\d{4,6})/);
                                if (m1) { firstLine = Number(m1[1]); break; }
                            }
                        }
                    }
                    return {
                        phase: m?.dataset.phase,
                        liveBottom: m?.dataset.liveBottom,
                        top: p?.scrollTop,
                        scrollH: p?.scrollHeight,
                        clientH: p?.clientHeight,
                        gap: p ? Math.round(p.scrollHeight - p.scrollTop - p.clientHeight) : null,
                        rows: h?.children.length || 0,
                        canvasHidden: !!c?.hidden,
                        latestHidden: !!l?.hidden,
                        rect: r ? {top: Math.round(r.top), bottom: Math.round(r.bottom)} : null,
                        firstLine,
                        screenText: c?.getAttribute('aria-label') || '',
                    };
                }''')

            cdp = context.new_cdp_session(page)

            def touch_drag(y0, y1, steps=12, step_ms=14, settle=0.4):
                cdp.send('Input.dispatchTouchEvent', {'type': 'touchStart', 'touchPoints': [{'x': 195, 'y': y0, 'id': 1}]})
                for i in range(1, steps + 1):
                    time.sleep(step_ms / 1000.0)
                    y = y0 + (y1 - y0) * i / steps
                    cdp.send('Input.dispatchTouchEvent', {'type': 'touchMove', 'touchPoints': [{'x': 195, 'y': y, 'id': 1}]})
                time.sleep(0.02)
                cdp.send('Input.dispatchTouchEvent', {'type': 'touchEnd', 'touchPoints': []})
                time.sleep(settle)
                # Wait for native momentum to settle completely
                prev_top = snap()['top']
                for _ in range(15):
                    time.sleep(0.08)
                    cur_top = snap()['top']
                    if abs(cur_top - prev_top) < 1:
                        break
                    prev_top = cur_top
                return snap()

            # 1. Streaming session: initial live bottom -> monotonic upward drags -> pinned during live output & resize -> return to live.
            open_session(stream_sid)
            page.wait_for_function('() => document.querySelectorAll(".terminal-history-row").length >= 300', timeout=15000)
            time.sleep(0.5)
            s0 = snap()
            assert s0['phase'] == 'ready' and s0['liveBottom'] == 'true' and s0['latestHidden'], s0
            page.screenshot(path=str(evidence / 'scroll-initial-bottom.png'))
            y_top = s0['rect']['top'] + 80
            y_bot = s0['rect']['bottom'] - 80

            # Monotonic upward touch drags (finger moves down y_top -> y_bot => scrollTop strictly decreases, line numbers strictly decrease).
            seen_lines = []
            prev_top = s0['top']
            for idx in range(5):
                sk = touch_drag(y_top, y_bot)
                assert sk['top'] < prev_top - 100, f'drag {idx+1} did not scroll up monotonically: {prev_top} -> {sk["top"]}'
                assert not sk['latestHidden'], f'Latest button must show while scrolled up: {sk}'
                if sk['firstLine'] is not None:
                    if seen_lines:
                        assert sk['firstLine'] < seen_lines[-1], f'line number jumped forward/teleported: {seen_lines[-1]} -> {sk["firstLine"]}'
                    seen_lines.append(sk['firstLine'])
                prev_top = sk['top']
            assert len(seen_lines) >= 3, f'expected monotonic line numbers while scrolling up: {seen_lines}'
            page.screenshot(path=str(evidence / 'scroll-up-history-pinned.png'))

            # Stay pinned while continuous live output arrives AND viewport resizes (keyboard / address bar).
            pinned = snap()
            time.sleep(1.5)
            after_live = snap()
            assert abs(after_live['top'] - pinned['top']) <= 2, f'live output moved pinned scrollTop: {pinned} -> {after_live}'
            assert after_live['firstLine'] == pinned['firstLine'], f'live output changed visible line: {pinned} -> {after_live}'
            page.set_viewport_size({'width': 390, 'height': 540})
            time.sleep(0.4)
            after_shrink = snap()
            page.set_viewport_size({'width': 390, 'height': 844})
            time.sleep(0.4)
            after_grow = snap()
            assert abs(after_shrink['top'] - pinned['top']) <= 2 and abs(after_grow['top'] - pinned['top']) <= 2, (pinned, after_shrink, after_grow)
            assert after_grow['firstLine'] == pinned['firstLine'], (pinned, after_grow)

            # Tap Latest output -> resumes live follow-tail automatically.
            page.click('.terminal-latest')
            time.sleep(0.4)
            live_back = snap()
            assert live_back['liveBottom'] == 'true' and live_back['latestHidden'] and live_back['gap'] <= 2, live_back
            before_screen = live_back['screenText']
            time.sleep(0.8)
            live_later = snap()
            assert live_later['liveBottom'] == 'true' and live_later['gap'] <= 2, live_later
            assert live_later['screenText'] != before_screen or 'LIVE_' in live_later['screenText'], 'live output resumed following'
            page.screenshot(path=str(evidence / 'scroll-returned-live.png'))

            # 2. 20k-line session: cross Hot -> Cold boundary without duplicate/forward teleportation, then smoothly scroll back down.
            open_session(config['terminalSession'])
            page.wait_for_function('() => document.querySelectorAll(".terminal-history-row").length >= 900', timeout=15000)
            time.sleep(0.4)
            s20 = snap()
            first_up = touch_drag(y_top, y_bot)
            assert first_up['top'] < s20['top'], f'first drag on 20k session jumped backwards: {s20["top"]} -> {first_up["top"]}'
            cold_lines = []
            reached_windowed = False
            for _ in range(24):
                sk = touch_drag(y_top, y_bot, steps=8, step_ms=10, settle=0.25)
                if sk['firstLine'] is not None:
                    if cold_lines:
                        assert sk['firstLine'] <= cold_lines[-1], f'cold scroll teleported forward: {cold_lines[-1]} -> {sk["firstLine"]}'
                    cold_lines.append(sk['firstLine'])
                if sk['canvasHidden']:
                    reached_windowed = True
                if reached_windowed and cold_lines and cold_lines[-1] < 19600:
                    break
            assert reached_windowed, f'expected to cross into windowed cold history: {cold_lines}'
            assert cold_lines and cold_lines[-1] < 19600, f'expected deduplicated cold lines below 19600: {cold_lines}'
            page.screenshot(path=str(evidence / 'scroll-cold-history.png'))
            # Gently scroll DOWN from windowed cold history: must advance monotonically without teleporting to live bottom.
            down_start = snap()
            down_step = touch_drag(y_top + 260, y_top + 100, steps=10, step_ms=14, settle=0.3)
            assert down_step['firstLine'] is None or down_start['firstLine'] is None or down_step['firstLine'] >= down_start['firstLine'], (down_start, down_step)
            assert down_step['liveBottom'] != 'true', f'gentle downward drag in windowed history teleported to live bottom: {down_step}'

            unhandled = page.evaluate('() => globalThis._unhandled')
            notices = page.evaluate('() => globalThis._notices')
            assert not console_errors, f'console errors: {console_errors}'
            assert not page_errors, f'page errors: {page_errors}'
            assert not unhandled, f'unhandled rejections: {unhandled}'
            assert not notices, f'unexpected notices: {notices}'
            browser.close()
        print('PASS: Playwright 390x844 mobile touch scroll E2E (0 errors, monotonic drag, pinned live/resize, cold deduplication, bidirectional window, follow-tail resume)')
    finally:
        try:
            cli('kill', str(stream_sid))
        except Exception:
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--address', default='127.0.0.1:0')
    parser.add_argument('--browser-only',action='store_true',help='Run live PWA checks without launching or measuring the native app')
    parser.add_argument('--evidence', default='/tmp/m9-live-qa')
    args = parser.parse_args()
    if ':' not in args.address:
        args.address += ':0'
    root = Path(__file__).resolve().parents[3]
    target = Path(os.environ.get('CARGO_TARGET_DIR', root / 'target'))
    krilld = Path(os.environ.get('KRILLD_BIN', str(target / 'debug/krilld')))
    krill = Path(os.environ.get('KRILL_BIN', str(krilld.with_name('krill'))))
    evidence = Path(args.evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    session = f'm9-live-{os.getpid()}'
    second_session = session + '-second'
    with tempfile.TemporaryDirectory(prefix='m9live-') as scratch:
        metadata = Path(scratch) / 'fixture.json'
        env = dict(os.environ, M9_BROWSER_ADDRESS=args.address, M9_BROWSER_METADATA=str(metadata), M9_LIVE_QA='1', KRILL_TELEMETRY_DISABLED='1', KRILL_ERROR_REPORTING_DISABLED='1')
        app = None
        with (evidence / 'fixture.log').open('w+') as log:
            fixture = subprocess.Popen(['node', str(Path(__file__).with_name('mobile_pwa.cjs')), str(krilld), '--keep'], env=env, stdout=log, stderr=log)
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
                    return subprocess.check_output([str(krill), *args], env=dict(os.environ, KRILL_STATE_DIR=config['dir'], KRILL_TELEMETRY_DISABLED='1', KRILL_ERROR_REPORTING_DISABLED='1'), text=True, timeout=30)
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
                    app=subprocess.Popen([str(krilld.with_name('krill-app'))],env=dict(os.environ,KRILL_STATE_DIR=config['dir'],HOME=str(Path(config['dir'])/'home'),KRILL_LOG_DIR=str(evidence/'process-logs'),KRILL_TELEMETRY_DISABLED='1',KRILL_ERROR_REPORTING_DISABLED='1'),cwd=config['repo'],stdout=app_log,stderr=app_log)
                    time.sleep(12)
                    assert app.poll() is None, f'native app exited: {app.returncode}; see {evidence}/process-logs'
                    assert len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]) == expected_ptys
                baseline = footprint(config['daemon'],'daemon-baseline')
                init=Path(scratch)/'sockets.js'
                init.write_text('localStorage.setItem("krill-mobile-error-reporting-v1","");localStorage.setItem("krill-mobile-ui-v1",JSON.stringify({fitToPhone:false,language:"en"}));')
                browser('--init-script',str(init),'open',config['url'])
                browser('set','viewport','390','844')
                wait('KrillMobile.store.connection.phase==="ready"')
                # terminalConnect may create a spare warm socket after returning
                # the live stream, so record the stream rather than the last socket.
                evaluate('(()=>{const transport=KrillMobile.app.transport,connect=transport.terminalConnect;transport.terminalConnect=function(...args){const result=connect.apply(this,args);globalThis.qaTerminalSocket=result.socket;return result;};return true;})()')
                terminal = config['terminalSession']
                assert len([line for line in cli('ls').splitlines() if line.split('\t')[0].isdigit()]) == expected_ptys
                # Select via its actual card, then render the real screen stream.
                wait(f'KrillMobile.store.sessions.has({terminal})')
                evaluate(f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({terminal}).groupKey)+"/s/{terminal}");true')
                wait('document.querySelector(".terminal-grid").getAttribute("aria-label").includes("LIVE RED")')
                original = size()
                assert evaluate('document.querySelector(".terminal-grid").getAttribute("aria-label").includes("한")')
                assert evaluate('document.querySelector(".terminal-grid").getAttribute("aria-label").normalize("NFC").includes("é")')
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
                # Record the close event before automatic reconnect can replace
                # the transient status. Leave the next attach at desktop size.
                evaluate('KrillMobile.store.preferences.fitToPhone=false;globalThis.qaDisconnectPhase=null;qaTerminalSocket.addEventListener("close",()=>{qaDisconnectPhase=document.querySelector(".terminal-mount")?.dataset.phase;},{once:true});qaTerminalSocket.close();true')
                wait('qaDisconnectPhase==="disconnected"')
                evaluate('KrillMobile.app.navigate("/host/local");true')
                evaluate(f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({terminal}).groupKey)+"/s/{terminal}");true')
                wait(f'document.querySelector(".terminal-status")?.textContent.startsWith("{original["cols"]} ×")')
                # Lazy hot paging followed by zstd cold replay; only 1000 rows stay in the browser.
                # The fixture prints 20,000 numbered lines and the hot buffer
                # holds 1000 rows. Older numbers prove cold replay independently
                # of the user-facing history status wording.
                cold_history = 'Array.from(document.querySelector(".terminal-history").textContent.matchAll(/(\\d{6}) history 한글/g)).some(m=>Number(m[1])<19500)'
                for _ in range(35):
                    evaluate('KrillMobile.activeTerminal.loadHistory(true);true')
                    wait('!document.querySelector(".history-status").textContent.startsWith("Loading")')
                    if evaluate(cold_history):
                        break
                assert evaluate(cold_history), evaluate('({status:document.querySelector(".history-status").textContent,first:document.querySelector(".terminal-history").textContent.slice(0,300)})')
                assert evaluate('document.querySelector(".terminal-history").textContent.split("\\n").length<=1001')
                assert evaluate('document.querySelector(".terminal-history").textContent.includes("history 한글")')
                browser('screenshot',str(evidence/'cold-history.png'))
                # The URL used by ntfy/Telegram/Discord reopens exactly this session.
                browser('open',config['base']+f'/?session={terminal}')
                wait(f'location.hash.endsWith("/s/{terminal}") && document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("KEYS:15") && document.querySelectorAll(".terminal-history-row").length>=900')
                assert evaluate('new URLSearchParams(location.search).get("session")') is None
                assert not evaluate('document.documentElement.scrollWidth>innerWidth')
                browser('screenshot',str(evidence/'deep-link.png'))
                assert not browser('errors').strip()
                # Pair a second device and keep its workflow + screen streams live too.
                second_offer = json.loads(cli('mobile','pair','--address',config['base'].split('://')[1],'--json'))['PairingOffer']['url']
                def second(*args):
                    result = subprocess.run(['agent-browser','--session',second_session,*args],text=True,capture_output=True,timeout=45)
                    assert result.returncode == 0, result.stdout + result.stderr
                    return result.stdout
                second('open',second_offer)
                second('set','viewport','390','844')
                second('wait','--fn','KrillMobile.store.connection.phase==="ready"')
                second('wait','--fn',f'KrillMobile.store.sessions.has({terminal})')
                second('eval',f'KrillMobile.app.navigate("/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({terminal}).groupKey)+"/s/{terminal}");true')
                second('wait','--fn','document.querySelector(".terminal-grid")?.getAttribute("aria-label").includes("KEYS:15") && document.querySelectorAll(".terminal-history-row").length>=900')
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
                verify_touch_scroll_ux(config, cli, evidence)
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

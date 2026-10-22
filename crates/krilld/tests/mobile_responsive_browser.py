"""Chromium responsive matrix against the isolated mobile_pwa.cjs LAN fixture.

Run with CARGO_TARGET_DIR pointing at the actual Cargo target directory.
--phase before records problems without failing; after enforces the contract.
Screenshots and measured geometry are kept for every viewport/state pair.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from mobile_browser import until


VIEWPORTS = [
    (320, 740), (390, 844), (430, 932), (844, 390),
    (344, 882), (690, 830), (830, 690), (1080, 790), (790, 1080),
    (744, 1133), (820, 1180), (1180, 820), (1032, 1376), (1376, 1032),
    (507, 1180), (375, 1024),
]

# Check scroll containers as well as the document: overflow:hidden on the shell
# otherwise conceals content that is clipped inside flex children.
MEASURE = """(()=>{
 const rect=n=>{const r=n.getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height,right:r.right,bottom:r.bottom};};
 const visible=n=>!!n.getClientRects().length&&getComputedStyle(n).visibility!=='hidden';
 const dialog=document.querySelector('dialog[open]'),scope=dialog||document.querySelector('.scanner')||document.querySelector('#route-root');
 const controls=[...scope.querySelectorAll('button,summary,.key-label')].filter(visible).map(n=>({name:n.getAttribute('aria-label')||n.textContent.trim(),...rect(n)}));
 const reading=[...document.querySelectorAll('.home-list,.workspace-list,.settings-list,.detail-list,.connection-log')].filter(visible).map(n=>({name:n.className,...rect(n)}));
 const overflow=[...document.querySelectorAll('#route-root,.screen,.home-list,.workspace-list,.settings-list,.detail-list,.connection-log,.chat-view,.chat-feed,.command-dock')].filter(visible).filter(n=>n.scrollWidth>n.clientWidth+1).map(n=>({name:n.className||n.id,scroll:n.scrollWidth,client:n.clientWidth}));
 const dock=document.querySelector('.terminal-mount:not([hidden]) .command-dock');
 const conversation=document.querySelector('.chat-view');
 const headerControls=[...document.querySelectorAll('.page-header button,.home-topbar button')].filter(visible).map(rect);
 return {width:innerWidth,height:innerHeight,scrollWidth:document.documentElement.scrollWidth,controls,reading,overflow,headerControls,conversation:conversation?rect(conversation):null,sheet:dialog?rect(dialog):null,dock:dock?rect(dock):null};
})()"""


def problems(m):
    issues = []
    if m['scrollWidth'] > m['width'] or m['overflow']:
        issues.append('horizontal overflow')
    small = [c['name'] for c in m['controls'] if c['w'] < 43.9 or c['h'] < 43.9]
    if small:
        issues.append('touch target <44: ' + ', '.join(small))
    for item in m['reading']:
        limit = 960 if 'workspace-list' in item['name'] else 720
        if m['width'] >= 600 and item['w'] > limit + 1:
            issues.append(f'reading width {item["name"]}: {item["w"]:.0f}>{limit}')
    if m['sheet'] and m['width'] >= 600:
        if m['sheet']['w'] > 513:
            issues.append('sheet width >512')
        # Very short landscape screens keep the full available height scrollable.
        if abs(m['sheet']['y'] - (m['height'] - m['sheet']['h']) / 2) > 2:
            issues.append('sheet not centered')
    if m['dock'] and m['dock']['bottom'] > m['height'] + 1:
        issues.append('terminal dock below viewport')
    if m.get('conversation') and m['conversation']['w'] > 1041:
        issues.append('conversation width >1040')
    if any(c['x'] < -1 or c['right'] > m['width'] + 1 for c in m.get('headerControls', [])):
        issues.append('header control clipped')
    return issues


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--address', required=True)
    parser.add_argument('--phase', choices=['before', 'after'], default='after')
    parser.add_argument('--screenshots')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    target = Path(os.environ['CARGO_TARGET_DIR'])
    shots = Path(args.screenshots or f'/tmp/krill-responsive/{args.phase}').resolve()
    shots.mkdir(parents=True, exist_ok=True)
    session = f'mobile-responsive-{os.getpid()}'
    results, resizing = [], []
    with tempfile.TemporaryDirectory(prefix='mobile-responsive-') as scratch:
        metadata = Path(scratch) / 'fixture.json'
        env = dict(os.environ, M9_UI_QA='1', M9_BROWSER_ADDRESS=args.address,
                   M9_BROWSER_METADATA=str(metadata))
        with (shots / 'fixture.log').open('w+') as log:
            fixture = subprocess.Popen(['node', str(Path(__file__).with_name('mobile_pwa.cjs')),
                                        str(target / 'debug/krilld'), '--keep'],
                                       env=env, stdout=log, stderr=log)
            try:
                until(metadata.exists, 90)
                cfg = json.loads(metadata.read_text())

                def browser(*command):
                    r = subprocess.run(['agent-browser', '--session', session, *command],
                                       capture_output=True, text=True, timeout=45)
                    assert r.returncode == 0, r.stdout + r.stderr
                    return r.stdout

                def evaluate(script):
                    return json.loads(browser('eval', '--json', script))['data']['result']

                def wait(script):
                    browser('wait', '--fn', script)

                def settle():
                    evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(()=>r(true))))')

                def navigate(route):
                    evaluate(f'KrillMobile.app.navigate({json.dumps(route)});true')
                    settle()

                def click(selector):
                    evaluate(f'document.querySelector({json.dumps(selector)}).click();true')
                    settle()

                def named(label):
                    evaluate(f'Array.from(document.querySelectorAll("button")).find(n=>n.getAttribute("aria-label")==={json.dumps(label)}).click();true')
                    settle()

                def close():
                    click('.sheet-cancel')

                def capture(name, width, height):
                    m = evaluate(MEASURE)
                    m.update(viewport=f'{width}x{height}', screen=name)
                    m['issues'] = problems(m)
                    results.append(m)
                    folder = shots / m['viewport']
                    folder.mkdir(exist_ok=True)
                    browser('screenshot', str(folder / f'{name}.png'))
                    (shots / 'measurements.json').write_text(json.dumps(results, indent=2))

                # First-launch states before consuming the one-time pairing ticket.
                init = Path(scratch) / 'preferences.js'
                init.write_text('localStorage.setItem("krill-mobile-ui-v1",JSON.stringify({language:"en",appearance:"dark"}));')
                browser('--init-script', str(init), 'open', cfg['base'])
                wait('!!globalThis.KrillMobile?.app')
                for width, height in VIEWPORTS:
                    browser('set', 'viewport', str(width), str(height))
                    navigate('/')
                    capture('home-empty', width, height)
                    named('Paste pairing link')
                    capture('pairing-link-sheet', width, height)
                    close()
                    named('Scan QR code')
                    capture('scanner-photo-fallback', width, height)
                    named('Close scanner')
                browser('open', cfg['url'])
                wait('!!document.querySelector(".pair-screen")')
                for width, height in VIEWPORTS:
                    browser('set', 'viewport', str(width), str(height))
                    capture('pair-confirm', width, height)
                named('Pair')
                wait('KrillMobile.store.connection.phase==="ready"')
                sid = cfg['session']
                wait(f'KrillMobile.store.sessions.has({sid})')
                route = evaluate(f'"/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({sid}).groupKey)+"/s/{sid}"')
                for width, height in VIEWPORTS:
                    browser('set', 'viewport', str(width), str(height))
                    navigate('/')
                    capture('home', width, height)
                    named('Desktop actions')
                    capture('desktop-actions', width, height)
                    named('Rename desktop')
                    capture('rename-desktop', width, height)
                    close()
                    navigate('/host/local')
                    wait('document.querySelectorAll(".workspace-row").length===4')
                    capture('workspaces', width, height)
                    click('.tree-toggle:not(.leaf)')
                    capture('workspace-terminals', width, height)
                    click('.workspace-toolbar .filter-chip')
                    capture('workspace-options', width, height)
                    close()
                    named('Search')
                    browser('fill', 'input[type=search]', 'no-matching-workspace')
                    capture('workspace-search-empty', width, height)
                    browser('fill', 'input[type=search]', '')
                    named('Search')
                    navigate(route)
                    wait('!!KrillMobile.activeTerminal && document.querySelector(".terminal-mount:not([hidden])")?.dataset.phase==="ready"')
                    evaluate('KrillMobile.activeTerminal.setInputMode("direct");true')
                    settle()
                    capture('session-direct', width, height)
                    named('Terminal actions')
                    capture('terminal-actions', width, height)
                    named('Send prompt')
                    capture('prompt-dialog', width, height)
                    close()
                    named('Terminal actions')
                    named('Select text')
                    capture('terminal-text', width, height)
                    close()
                    named('Terminal actions')
                    named('Close terminal')
                    capture('close-confirm', width, height)
                    close()
                    named('Create Tab')
                    capture('create-tab', width, height)
                    close()
                    evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
                    settle()
                    capture('session-buffered', width, height)
                    evaluate(f'KrillMobile.store.chatCache?.delete({sid});true')
                    named('Chat')
                    wait('!!document.querySelector(".chat-feed")')
                    capture('chat-empty', width, height)
                    # Real renderer, deterministic content: the shell fixture has no
                    # agent transcript. Seed only its cache; no production QA hooks.
                    evaluate(f'''(()=>{{const e=KrillMobile.store.chatCache.get({sid});e.items=[{{kind:'user',text:'폴드를 펼친 뒤에도 입력을 이어갑니다. '.repeat(8)}},{{kind:'assistant',text:'Long readable conversation on a wide screen. '.repeat(28)}}];e.bound=true;return true;}})()''')
                    named('Term')
                    named('Chat')
                    capture('chat-content', width, height)
                    named('Term')
                    navigate('/accounts')
                    wait('!!document.querySelector(".account-row")')
                    capture('accounts', width, height)
                    navigate('/orchestration')
                    capture('orchestration', width, height)
                    navigate(f'/git/{sid}')
                    wait('!!document.querySelector(".file-row")')
                    click('.file-row')
                    wait('document.querySelector("#git-patch").textContent.includes("+after")')
                    capture('git', width, height)
                    for subpage, name in [('', 'settings'), ('/terminal', 'terminal-settings'),
                                          ('/about', 'about')]:
                        navigate('/settings' + subpage)
                        capture(name, width, height)
                    navigate('/settings')
                    evaluate('KrillMobile.install.prompt=null;KrillMobile.install.show();true')
                    wait('document.getElementById("action-sheet").open')
                    capture('install-dialog', width, height)
                    close()
                    navigate('/diagnostics')
                    capture('connection-log', width, height)
                    # Connection callbacks own this DOM in production. Pin a long
                    # reconnect message between ticks to exercise wrapping geometry.
                    evaluate('''(()=>{const b=document.getElementById('connection-banner');b.className='connection-banner';b.hidden=false;b.innerHTML='<span>Reconnecting to desktop — 연결이 복구되면 계속할 수 있습니다.</span><button class="banner-action">Retry</button>';const n=document.getElementById('notice');n.textContent='Connection interrupted. 연결 상태를 확인하고 다시 시도하세요. '.repeat(4);n.hidden=false;return true;})()''')
                    capture('banner-toast', width, height)
                    evaluate('document.getElementById("connection-banner").hidden=true;document.getElementById("notice").hidden=true;true')
                    print(f'{width}x{height}: recorded', flush=True)

                # Same live terminal survives fold/unfold, rotation, and narrow
                # split-view transitions. Check the acknowledged grid, not CSS only.
                navigate(route)
                wait('!!KrillMobile.activeTerminal')
                evaluate('KrillMobile.activeTerminal.fitToPhone(true);true')
                browser('fill', '.command-input', 'fold draft 한글')
                resize_route = evaluate('location.hash')
                for width, height in [(390, 844), (690, 830), (1080, 790), (1376, 1032),
                                      (507, 1180), (320, 740), (844, 390), (390, 844)]:
                    browser('set', 'viewport', str(width), str(height))
                    wait('Math.round(parseFloat(document.querySelector(".terminal-grid").style.width)/8)===Math.floor((document.querySelector(".terminal-scroll").clientWidth-2)/8)')
                    resizing.append(evaluate('({width:innerWidth,height:innerHeight,cols:Math.round(parseFloat(document.querySelector(".terminal-grid").style.width)/8),rows:Math.round(parseFloat(document.querySelector(".terminal-grid").style.height)/18)})'))
                    assert evaluate('document.querySelector(".command-input").value') == 'fold draft 한글'
                    assert evaluate('location.hash') == resize_route
                assert all((b['cols'] > a['cols']) == (b['width'] > a['width'])
                           for a, b in zip(resizing, resizing[1:])), resizing
                (shots / 'resize.json').write_text(json.dumps(resizing, indent=2))
                if args.phase == 'after':
                    # Readability and wrapping also survive localized large text.
                    navigate('/settings')
                    click('[data-focus-key="preference-language-ko"]')
                    click('[data-focus-key="preference-appearance-light"]')
                    click('[data-focus-key="preference-uiTextScale-1.5"]')
                    for width, height in VIEWPORTS:
                        browser('set', 'viewport', str(width), str(height))
                        for path, name in [('/settings', 'settings'), ('/settings/terminal', 'terminal-settings'),
                                           ('/settings/about', 'about'), ('/host/local', 'workspaces'),
                                           ('/accounts', 'accounts'), (route, 'session')]:
                            navigate(path)
                            if name == 'accounts':
                                wait('!!document.querySelector(".account-row")')
                            capture(name + '-ko-large-light', width, height)
                        click('.session .mode-toggle')
                        capture('chat-ko-large-light', width, height)
                        click('.session .page-header .icon-button:last-child')
                        capture('dialog-ko-large-light', width, height)
                        close()
                        click('.session .mode-toggle')
                assert not browser('errors').strip(), browser('errors')
            finally:
                subprocess.run(['agent-browser', '--session', session, 'close'], capture_output=True, timeout=15)
                fixture.terminate()
                try:
                    fixture.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait()
                if fixture.returncode not in (0, -15):
                    log.seek(0)
                    print(log.read())
    failed = [r for r in results if r['issues']]
    print(f'{args.phase}: {len(results)} screenshots; {len(failed)} states with issues; {len(resizing)} live resizes')
    if args.phase == 'after':
        assert not failed, '\n'.join(f'{r["viewport"]}/{r["screen"]}: {r["issues"]}' for r in failed)
        print('PASS responsive Chromium LAN matrix')


if __name__ == '__main__':
    main()

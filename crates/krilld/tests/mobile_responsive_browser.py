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
 const controls=[...scope.querySelectorAll('button,summary,.key-label')].filter(visible).map(n=>({name:n.getAttribute('aria-label')||n.textContent.trim(),compact:!!n.closest('.session.screen>.page-header,.session-tabs,.command-dock,.quick-commands-list')||n.classList.contains('quick-command-add'),...rect(n)}));
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
    # The session header, tabs and terminal dock are compact so the terminal body keeps the most rows.
    small = [c['name'] for c in m['controls'] if min(c['w'], c['h']) < (33.9 if c['compact'] else 43.9)]
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
                init.write_text('if(!localStorage.getItem("krill-mobile-ui-v1"))localStorage.setItem("krill-mobile-ui-v1",JSON.stringify({language:"en",appearance:"dark"}));const NativeSocket=WebSocket;globalThis.tabletSockets=[];globalThis.WebSocket=class extends NativeSocket{constructor(...args){super(...args);tabletSockets.push(this);}};')
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
                    evaluate(f'''(()=>{{const e=KrillMobile.store.chatCache.get({sid});e.items.push({{kind:'reasoning',text:'Inspect the fold transition.'}},{{kind:'tool_call',tool_name:'read_file',title:'Read a long workspace path '.repeat(8),text:'src/mobile/app.css'}},{{kind:'tool_result',text:'Responsive layout verified.'}});return true;}})()''')
                    named('Term')
                    named('Chat')
                    evaluate('document.querySelectorAll(".chat-feed details").forEach(n=>n.open=true);true')
                    capture('chat-disclosures', width, height)
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
                    resizing.append(evaluate('({width:innerWidth,height:innerHeight,portWidth:document.querySelector(".terminal-scroll").clientWidth,cols:Math.round(parseFloat(document.querySelector(".terminal-grid").style.width)/8),rows:Math.round(parseFloat(document.querySelector(".terminal-grid").style.height)/18)})'))
                    assert evaluate('document.querySelector(".command-input").value') == 'fold draft 한글'
                    assert evaluate('location.hash') == resize_route
                assert all((b['cols'] > a['cols']) == (b['portWidth'] > a['portWidth'])
                           for a, b in zip(resizing, resizing[1:])), resizing
                (shots / 'resize.json').write_text(json.dumps(resizing, indent=2))
                # Real history must keep its anchor when folds arrive without
                # waiting for a PTY resize ACK. Use an unmounted fixture terminal
                # so its first history fetch includes the output we just produced.
                fold_sid = cfg['extraSessions'][0]
                history_text = '\n'.join(f'fold-history-{i:03d}' for i in range(120)) + '\n'
                evaluate(f'(async()=>{{await KrillMobile.app.transport.rpc({{action:"prompt",session:{fold_sid},text:{json.dumps(history_text)}}});return true;}})()')
                def history_ready():
                    output = subprocess.check_output([str(target / 'debug/krill'), 'read', str(fold_sid)],
                                                     env=dict(os.environ, KRILL_STATE_DIR=cfg['dir']), text=True)
                    return 'fold-history-119' in output
                until(history_ready)
                fold_route = evaluate(f'"/host/local/group/"+encodeURIComponent(KrillMobile.store.sessions.get({fold_sid}).groupKey)+"/s/{fold_sid}"')
                browser('set', 'viewport', '344', '882')
                navigate(fold_route)
                wait('document.querySelector(".terminal-mount:not([hidden])")?.dataset.phase==="ready"')
                evaluate('KrillMobile.activeTerminal.setInputMode("buffered");KrillMobile.activeTerminal.fitToPhone(true);true')
                wait('document.querySelector(".terminal-mount:not([hidden]) .terminal-history").textContent.includes("fold-history-")')
                browser('fill', '.terminal-mount:not([hidden]) .command-input', 'rapid fold draft 한글')
                evaluate('document.activeElement.blur();document.querySelector(".terminal-mount:not([hidden]) .terminal-scroll").scrollTop=180;globalThis.foldTerminal=KrillMobile.activeTerminal;true')
                settle()
                anchor = evaluate('document.querySelector(".terminal-mount:not([hidden]) .terminal-scroll").scrollTop')
                history = evaluate('document.querySelector(".terminal-mount:not([hidden]) .terminal-history").textContent')
                assert anchor > 0
                fold_hash = evaluate('location.hash')
                browser('screenshot', str(shots / 'rapid-fold-before.png'))
                folds = []
                for width, height in [(1180, 820), (830, 690), (1032, 1376), (344, 882)] * 3:
                    browser('set', 'viewport', str(width), str(height))
                    sample = evaluate('(()=>{const root=document.querySelector(".terminal-mount:not([hidden])");return {width:innerWidth,top:root.querySelector(".terminal-scroll").scrollTop,history:root.querySelector(".terminal-history").textContent,draft:root.querySelector(".command-input").value,hash:location.hash,sameTerminal:foldTerminal===KrillMobile.activeTerminal};})()')
                    assert sample.pop('history') == history
                    assert sample.pop('hash') == fold_hash
                    assert sample['sameTerminal'] and sample['draft'] == 'rapid fold draft 한글'
                    assert abs(sample['top'] - anchor) <= 1, sample
                    folds.append(sample)
                for width, height in [(690, 830), (344, 882)]:
                    browser('set', 'viewport', str(width), str(height))
                    wait('(()=>{const p=document.querySelector(".terminal-mount:not([hidden]) .terminal-scroll"),g=p.querySelector(".terminal-grid");return Math.round(parseFloat(g.style.width)/8)===Math.floor((p.clientWidth-2)/8);})()')
                    sample = evaluate('(()=>{const p=document.querySelector(".terminal-mount:not([hidden]) .terminal-scroll"),g=p.querySelector(".terminal-grid");return {width:innerWidth,cols:Math.round(parseFloat(g.style.width)/8),top:p.scrollTop};})()')
                    assert abs(sample['top'] - anchor) <= 1, sample
                    folds.append(sample)
                (shots / 'rapid-fold.json').write_text(json.dumps(folds, indent=2))
                browser('screenshot', str(shots / 'rapid-fold-after.png'))
                print('PASS rapid fold x12: live grid, draft, terminal identity, history and scroll anchor', flush=True)
                # Tablet shell, independent terminal panes and real daemon lifetime.
                def cli(*command):
                    return subprocess.check_output([str(target / 'debug/krill'), *command],
                        env=dict(os.environ, KRILL_STATE_DIR=cfg['dir']), text=True)

                def pane_ready():
                    wait('document.querySelectorAll(".terminal-mount[data-phase=ready]").length===2')
                    wait('[...document.querySelectorAll(".terminal-mount .terminal-scroll")].every(p=>Math.round(parseFloat(p.querySelector(".terminal-grid").style.width)/8)===Math.floor((p.clientWidth-2)/8))')

                def direction(value):
                    click('.split-direction')
                    assert evaluate(f'document.querySelector(".split-direction-choice[data-direction={value}]").getAttribute("aria-pressed")') in ('true', 'false')
                    click(f'.split-direction-choice[data-direction={value}]')
                    pane_ready()

                def pane_sizes():
                    return evaluate('[...document.querySelectorAll(".terminal-pane")].map(n=>{const p=n.querySelector(".terminal-scroll"),g=p.querySelector(".terminal-grid");return {width:n.clientWidth,portWidth:p.clientWidth,portHeight:p.clientHeight,cols:Math.round(parseFloat(g.style.width)/8),rows:Math.round(parseFloat(g.style.height)/18)};})')

                other = cfg['extraSessions'][0]
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
                            if name == 'session':
                                evaluate('document.querySelectorAll(".terminal-scroll").forEach(n=>n.scrollTop=0);true')
                            capture(name + '-ko-large-light', width, height)
                            if name == 'session' and width >= 1000:
                                named('분할')
                                evaluate(f'[...document.querySelectorAll(".sheet-action")].find(n=>n.textContent.endsWith("#{other}")).click();true')
                                pane_ready()
                                capture('tablet-split-ko-large-light', width, height)
                                click('.secondary-pane .pane-close')
                        click('.session .mode-toggle')
                        # Native details toggle events can land after a remount.
                        # Pin disclosure state and scroll before geometry captures.
                        evaluate('document.querySelectorAll(".chat-feed details").forEach(n=>n.open=true);document.querySelector(".chat-feed").scrollTop=0;true')
                        settle()
                        capture('chat-ko-large-light', width, height)
                        click('.session .page-header .icon-button:last-child')
                        capture('dialog-ko-large-light', width, height)
                        close()
                        click('.session .mode-toggle')
                # Keep the original 608 captures before any split reload or new input.
                navigate('/settings')
                click('[data-focus-key="preference-language-en"]')
                click('[data-focus-key="preference-appearance-dark"]')
                click('[data-focus-key="preference-uiTextScale-1"]')
                tablet = []
                evaluate('KrillMobile.store.ui.search="no-matching-workspace";KrillMobile.store.ui.filter="attention";true')
                for width, height in VIEWPORTS:
                    browser('set', 'viewport', str(width), str(height))
                    navigate(route)
                    wait('!!KrillMobile.activeTerminal')
                    assert evaluate('!!document.querySelector(".tablet-sidebar").getClientRects().length') == (width >= 1000)
                    if width < 1000:
                        continue
                    evaluate('globalThis.tabletSidebar=document.querySelector(".tablet-sidebar");true')
                    click(f'.tablet-sidebar .session-row[data-session="{other}"]')
                    wait(f'location.hash.endsWith("/s/{other}")')
                    assert evaluate('tabletSidebar===document.querySelector(".tablet-sidebar")')
                    assert evaluate(f'document.querySelector(".session-tab[aria-selected=true]").dataset.session==="{other}"')
                    navigate(route)
                    wait('!!KrillMobile.activeTerminal')
                    capture('tablet-sidebar', width, height)
                    tablet.append({'width': width, 'height': height, 'sidebarPreserved': True})
                evaluate('KrillMobile.store.ui.search="";KrillMobile.store.ui.filter="all";true')
                browser('set', 'viewport', '1180', '820')
                navigate(route)
                wait('document.querySelector(".terminal-mount")?.dataset.phase==="ready"')
                evaluate('globalThis.sidebarTerminal=KrillMobile.activeTerminal;true')
                assert evaluate('document.querySelector(".tablet-sidebar-toggle").title') == 'Hide session list'
                assert evaluate('document.querySelector(".tablet-sidebar-toggle path").getAttribute("d")') != evaluate('document.querySelector(".tablet-sidebar .page-header button path").getAttribute("d")')
                named('Hide session list')
                assert evaluate('!document.querySelector(".tablet-sidebar").getClientRects().length')
                assert evaluate('document.querySelector(".tablet-sidebar-toggle").title') == 'Show session list'
                assert evaluate('document.querySelector(".tablet-sidebar-toggle").getAttribute("aria-expanded")') == 'false'
                named('Show session list')
                browser('set', 'viewport', '830', '690')
                browser('set', 'viewport', '1180', '820')
                assert evaluate('sidebarTerminal===KrillMobile.activeTerminal')
                named('Split terminal')
                capture('tablet-split-picker', 1180, 820)
                assert not evaluate(f'[...document.querySelectorAll(".sheet-action")].some(n=>n.textContent.endsWith("#{sid}"))')
                evaluate(f'[...document.querySelectorAll(".sheet-action")].find(n=>n.textContent.endsWith("#{other}")).click();true')
                pane_ready()
                assert evaluate('getComputedStyle(document.querySelector(".terminal-pane[data-active=true]"),"::after").borderTopWidth') == '2px'
                assert evaluate('document.getElementById("notice").hidden || !document.getElementById("notice").textContent.includes("Wait for the live terminal")')
                assert evaluate('!document.querySelector(".primary-pane .command-dock").getClientRects().length && !!document.querySelector(".secondary-pane .command-dock").getClientRects().length')
                capture('tablet-split-columns-active-second', 1180, 820)
                assert evaluate('getComputedStyle(document.querySelector(".session-panels")).flexDirection') == 'row'
                assert evaluate('document.querySelectorAll(".is-split > .terminal-pane > .pane-header").length') == 2
                assert evaluate('[...document.querySelectorAll(".pane-title")].every(n=>n.querySelector(".status-dot")&&n.querySelector(".pane-label").textContent.includes(" · ")&&n.querySelector(".pane-active"))')
                assert evaluate('document.querySelector(".secondary-pane .pane-title").getAttribute("aria-pressed")') == 'true'
                assert evaluate('getComputedStyle(document.querySelector(".secondary-pane .pane-active")).visibility') == 'visible'
                assert evaluate(f'document.querySelector(`.session-tab[data-session="{sid}"]`).disabled')
                # A third PTY in the same workspace lets the tab strip replace either active pane.
                third = int(cli('new', '--cwd', cfg['repo'], '--', '/bin/sh', '-c', "printf 'TAB READY\\n'; exec /bin/cat"))
                wait(f'KrillMobile.store.sessions.has({third})')
                click(f'.session-tab[data-session="{third}"]')
                pane_ready()
                assert evaluate(f'document.querySelector(".secondary-pane").dataset.session==="{third}"')
                assert evaluate(f'document.querySelector(".primary-pane").dataset.session==="{sid}"')
                evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
                browser('fill', '.secondary-pane .command-input', 'active-secondary-tab-only')
                evaluate('document.querySelector(".secondary-pane .buffered-command").requestSubmit();true')
                until(lambda: 'active-secondary-tab-only' in cli('read', str(third)))
                assert 'active-secondary-tab-only' not in cli('read', str(sid))
                assert 'active-secondary-tab-only' not in cli('read', str(other))
                click(f'.session-tab[data-session="{other}"]')
                pane_ready()
                click('.primary-pane .pane-title')
                assert evaluate('document.querySelector(".primary-pane .pane-title").getAttribute("aria-pressed")') == 'true'
                assert evaluate(f'document.querySelector(`.session-tab[data-session="{other}"]`).disabled')
                click(f'.session-tab[data-session="{third}"]')
                pane_ready()
                assert evaluate(f'document.querySelector(".primary-pane").dataset.session==="{third}"')
                assert evaluate(f'document.querySelector(".secondary-pane").dataset.session==="{other}"')
                evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
                browser('fill', '.primary-pane .command-input', 'active-primary-tab-only')
                evaluate('document.querySelector(".primary-pane .buffered-command").requestSubmit();true')
                until(lambda: 'active-primary-tab-only' in cli('read', str(third)))
                assert 'active-primary-tab-only' not in cli('read', str(other))
                click(f'.session-tab[data-session="{sid}"]')
                pane_ready()
                click('.secondary-pane .pane-title')
                layouts = []
                for width, height in [(1032, 1376), (1080, 790), (1180, 820), (1376, 1032), (1040, 820), (1042, 820)]:
                    browser('set', 'viewport', str(width), str(height))
                    for value in ['auto', 'columns', 'rows']:
                        direction(value)
                        expected = 'columns' if (value == 'auto' and width - 320 >= 800) or (value == 'columns' and width - 320 >= 721) else 'rows'
                        assert evaluate('document.querySelector(".session-panels").dataset.layout') == expected
                        assert evaluate('document.querySelector(".split-direction").dataset.fallback') == str(value == 'columns' and expected == 'rows').lower()
                        sizes = pane_sizes()
                        for size in sizes:
                            assert size['cols'] == (size['portWidth'] - 2) // 8, size
                            assert size['rows'] == max(10, min(200, size['portHeight'] // 18)), size
                        layouts.append({'viewport': [width, height], 'direction': value, 'actual': expected, 'panes': sizes})
                        capture('tablet-direction-' + value, width, height)
                    direction('columns')
                    named('Hide session list')
                    pane_ready()
                    assert evaluate('document.querySelector(".session-panels").dataset.layout') == 'columns'
                    for size in pane_sizes():
                        assert size['cols'] == (size['portWidth'] - 2) // 8
                        assert size['rows'] == max(10, min(200, size['portHeight'] // 18))
                    named('Show session list')
                    pane_ready()
                (shots / 'directions.json').write_text(json.dumps(layouts, indent=2))
                direction('rows')
                browser('reload')
                pane_ready()
                assert evaluate('document.querySelector(".split-direction").dataset.direction') == 'rows'
                assert evaluate('document.querySelector(".session-panels").dataset.layout') == 'rows'
                direction('auto')
                browser('set', 'viewport', '1180', '820')
                pane_ready()
                second_handle = 'globalThis.secondHandle=KrillMobile.activeTerminal;true'
                evaluate(second_handle)
                evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
                browser('fill', '.secondary-pane .command-input', 'tablet-second-only')
                evaluate('document.querySelector(".secondary-pane .buffered-command").requestSubmit();true')
                until(lambda: 'tablet-second-only' in cli('read', str(other)))
                assert 'tablet-second-only' not in cli('read', str(sid))
                browser('click', '.primary-pane .terminal-scroll')
                assert evaluate('secondHandle!==KrillMobile.activeTerminal')
                evaluate('KrillMobile.activeTerminal.setInputMode("buffered");true')
                browser('fill', '.primary-pane .command-input', 'tablet-first-only')
                evaluate('document.querySelector(".primary-pane .buffered-command").requestSubmit();true')
                until(lambda: 'tablet-first-only' in cli('read', str(sid)))
                assert 'tablet-first-only' not in cli('read', str(other))
                browser('fill', '.primary-pane .command-input', 'primary fold draft 한글')
                evaluate('document.querySelector(".secondary-pane .command-input").value="second fold draft 한글";globalThis.firstHandle=KrillMobile.activeTerminal;globalThis.splitSocketCount=tabletSockets.filter(s=>s.readyState===1).length;true')
                capture('tablet-split-columns-active-first', 1180, 820)
                browser('set', 'viewport', '830', '690')
                wait('document.querySelectorAll(".terminal-mount[data-phase=ready]").length===1')
                wait('tabletSockets.filter(s=>s.readyState===1).length===splitSocketCount-1')
                assert evaluate('firstHandle===KrillMobile.activeTerminal')
                assert evaluate('document.querySelector(".primary-pane .command-input").value') == 'primary fold draft 한글'
                browser('set', 'viewport', '1180', '820')
                pane_ready()
                assert evaluate('firstHandle===KrillMobile.activeTerminal')
                assert evaluate('document.querySelector(".secondary-pane .command-input").value') == 'second fold draft 한글'
                browser('click', '.secondary-pane .terminal-scroll')
                evaluate('globalThis.secondHandle=KrillMobile.activeTerminal;true')
                browser('set', 'viewport', '830', '690')
                wait('document.querySelectorAll(".terminal-mount[data-phase=ready]").length===1')
                assert evaluate('secondHandle===KrillMobile.activeTerminal')
                browser('set', 'viewport', '1180', '820')
                pane_ready()
                assert evaluate('document.querySelector(".primary-pane .command-input").value') == 'primary fold draft 한글'
                browser('set', 'viewport', '1032', '1376')
                pane_ready()
                assert evaluate('getComputedStyle(document.querySelector(".session-panels")).flexDirection') == 'column'
                capture('tablet-split-rows', 1032, 1376)
                browser('reload')
                pane_ready()
                assert evaluate(f'document.querySelector(".secondary-pane").dataset.session==="{other}"')
                capture('tablet-split-restored', 1032, 1376)
                count_before_close = len(cli('ls').splitlines())
                click('.secondary-pane .pane-close')
                wait('document.querySelectorAll(".terminal-mount[data-phase=ready]").length===1')
                assert evaluate('!document.querySelector(".secondary-pane")')
                assert len(cli('ls').splitlines()) == count_before_close
                assert str(other) in [line.split('\t')[0] for line in cli('ls').splitlines()]
                assert 'tablet-second-only' in cli('read', str(other))
                named('Split terminal')
                evaluate(f'[...document.querySelectorAll(".sheet-action")].find(n=>n.textContent.endsWith("#{other}")).click();true')
                pane_ready()
                click('.primary-pane .pane-close')
                wait('document.querySelectorAll(".terminal-mount[data-phase=ready]").length===1')
                assert evaluate(f'location.hash.endsWith("/s/{other}")')
                assert len(cli('ls').splitlines()) == count_before_close
                assert 'tablet-first-only' in cli('read', str(sid))
                (shots / 'tablet.json').write_text(json.dumps({'sidebar': tablet, 'inputIsolated': True,
                    'paneCloseKeepsPty': True, 'narrowKeepsActiveHandle': True, 'splitRestored': True, 'activePaneTabs': True, 'symmetricHeaders': True, 'bothPaneCloseKeepCount': count_before_close}, indent=2))
                print('PASS tablet sidebar, split geometry, isolated input, PTY survival, folding drafts and reload', flush=True)
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

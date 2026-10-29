"""Chromium input/dock regression using the real adapter/CSS and an isolated wire fixture.

Run: python3 crates/krilld/tests/mobile_terminal_input_browser.py
IME and software-keyboard viewport events are simulated; rendering and keyboard
keystrokes run in Chromium. No existing user session or Rust build is required.
"""
import argparse
import functools
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', default='/tmp/krill-mobile-1534-input-browser')
    args = parser.parse_args()
    evidence = Path(args.evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    tests = Path(__file__).resolve().parent
    assets = tests.parent / 'src/mobile/assets'
    session = f'terminal-input-{os.getpid()}'

    def browser(*command):
        result = subprocess.run(['agent-browser', '--session', session, *command],
                                capture_output=True, text=True, timeout=45)
        assert result.returncode == 0, result.stdout + result.stderr
        return result.stdout

    def evaluate(script):
        return json.loads(browser('eval', '--json', script))['data']['result']

    def wait(script):
        browser('wait', '--fn', script)

    def shot(name):
        browser('screenshot', str(evidence / f'{name}.png'))

    with tempfile.TemporaryDirectory(prefix='krill-terminal-input-') as scratch:
        directory = Path(scratch)
        subprocess.run(['node', str(tests / 'mobile_terminal.cjs'), '--harness'],
                       env=dict(os.environ, MOBILE_TERMINAL_HARNESS_DIR=scratch), check=True)
        fixture = (directory / 'fixture.js').read_text().split(' globalThis.handle=')[0]
        (directory / 'fixture.js').write_text(fixture + """
globalThis.sentInputs=()=>socket.sent.filter(a=>a instanceof Uint8Array&&a[4]===0x13).map(a=>{const r=MkTerminalCodec.reader(a.slice(5));r.n();return r.text();});
globalThis.fire=(type,props={})=>{const e=new Event(type,{bubbles:true,cancelable:true});Object.assign(e,props);capture.dispatchEvent(e);};
globalThis.edit=(value,props={})=>{capture.value=value;capture.setSelectionRange(value.length,value.length);fire('input',props);};
globalThis.mountCase=async(id=7)=>{handle?.unmount();globalThis.handle=mountFixture(document.querySelector('main'),id,{preferences:{inputMode:'direct',fitToPhone:false},onPhoto:()=>{},onEditKeys:()=>{}});await Promise.resolve();globalThis.socket=sockets.at(-1);socket.handshake();socket.screen('Krill mobile input regression');socket.autoAck=true;globalThis.capture=document.querySelector('.live-capture');globalThis.inputEvents=[];for(const type of ['keydown','beforeinput','input'])capture.addEventListener(type,e=>{if(inputEvents.length<30)inputEvents.push({type:e.type,key:e.key,data:e.data,inputType:e.inputType,repeat:e.repeat,trusted:e.isTrusted,value:capture.value});});};
globalThis.handle=null;mountCase();
""")
        (directory / 'app.css').write_bytes((assets / 'app.css').read_bytes())
        (directory / 'index.html').write_text('''<!doctype html><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<link rel="stylesheet" href="/app.css"><div id="mobile-app"><div class="session screen">
<main class="terminal-mount"></main></div></div>
<script>globalThis.visualViewport=Object.assign(new EventTarget(),{height:innerHeight,offsetTop:0,scale:1});</script>
<script src="/fixture.js"></script>''')

        class QuietHandler(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0),
                    functools.partial(QuietHandler, directory=scratch))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        measurements = []
        try:
            browser('set', 'viewport', '390', '844')
            browser('open', f'http://127.0.0.1:{server.server_port}/')
            wait('document.querySelector(".terminal-mount")?.dataset.phase==="ready"')
            # Exercise the two dock toggles through actual browser clicks.
            browser('click', '.direct-toggle')
            assert evaluate('handle.getInputMode()') == 'buffered'
            assert evaluate('document.querySelector(".command-input").getClientRects().length>0')
            browser('fill', '.command-input', 'keep this draft')
            browser('click', '.direct-toggle')
            assert evaluate('handle.getInputMode()') == 'direct'
            assert evaluate('!document.querySelector(".command-input").getClientRects().length')
            assert evaluate('document.querySelector(".live-focus-target").getClientRects().length>0')
            browser('click', '.direct-toggle')
            assert evaluate('document.querySelector(".command-input").value') == 'keep this draft'
            browser('fill', '.command-input', '')
            browser('click', '.direct-toggle')
            browser('click', '.display-mode')
            assert evaluate('document.querySelector(".display-mode").disabled')
            assert evaluate('socket.sent.filter(a=>a[4]===0x17).at(-1)[5]') == 1
            evaluate('socket.frame(0x2c,MkTerminalCodec.v(0));true')
            wait('document.querySelector(".display-mode").ariaPressed==="true"')
            browser('click', '.display-mode')
            assert evaluate('[...socket.sent.filter(a=>a[4]===0x17).at(-1).slice(5)]') == [0, 2, 0]
            evaluate('socket.frame(0x2c,MkTerminalCodec.v(0));true')
            wait('document.querySelector(".display-mode").ariaPressed==="false"')
            # Touch questions against the real DOM, adapter and mobile CSS.
            for width, height in [(320, 740), (390, 844), (844, 390)]:
                browser('set', 'viewport', str(width), str(height))
                evaluate(f'mountCase({width+5000});true')
                wait('document.querySelector(".terminal-mount").dataset.phase==="ready"')
                evaluate('socket.question(["Which approach should we use?", "› 1. Keep the existing service (Recommended)", "  2. Create a new service", "  3. 직접 입력해서 다른 방법을 설명합니다", "Press enter to confirm or esc to cancel"]);true')
                wait('!document.querySelector(".terminal-question").hidden')
                box=evaluate('''(()=>{const p=document.querySelector('.terminal-question'),r=p.getBoundingClientRect(),buttons=[...p.querySelectorAll('button')];return {width:innerWidth,height:innerHeight,left:r.left,right:r.right,top:r.top,bottom:r.bottom,overflow:document.documentElement.scrollWidth>innerWidth,touch:buttons.every(b=>b.getBoundingClientRect().height>=44),text:buttons.map(b=>b.textContent)}})()''')
                assert not box['overflow'] and box['touch'] and box['left']>=0 and box['right']<=width and box['bottom']<=height, box
                browser('screenshot', str(evidence / f'{width}-question.png'))
                browser('click', '[data-testid="question-choice-3"]')
                wait('sentInputs().length===1')
                assert evaluate('sentInputs()')==['\x1b[B\x1b[B\r']
                assert evaluate('document.activeElement!==capture')
                assert evaluate('[...document.querySelectorAll(".question-choice")].every(b=>b.disabled)')
                evaluate('socket.question(["Pick features", "❯ ☐ Search", "  ☑ Export", "Space to select · Enter to submit"]);true')
                browser('click', '[data-testid="question-choice-1"]')
                assert evaluate('sentInputs().at(-1)')==' '
                evaluate('socket.question(["Pick features", "❯ ☑ Search", "  ☑ Export", "Space to select · Enter to submit"]);true')
                browser('screenshot', str(evidence / f'{width}-multi-question.png'))
                browser('click', '[data-testid="question-submit"]')
                assert evaluate('sentInputs().at(-1)')=='\r'
            browser('set', 'viewport', '390', '844')
            evaluate('mountCase(7);true')
            wait('document.querySelector(".terminal-mount").dataset.phase==="ready"')
            expected = ['escape', 'tab', 'enter', 'shiftTab', 'space', 'backspace',
                        'delete', 'arrowUp', 'arrowDown', 'arrowLeft', 'arrowRight']
            assert evaluate('[...document.querySelector(".keybar").children].slice(3,14).map(n=>n.dataset.keyId)') == expected
            assert evaluate('document.querySelector(".direct-toggle svg path").getAttribute("d")') == 'm6 17 5-5-5-5'
            browser('click', '.live-focus-target')
            browser('keyboard', 'type', 'hello')
            wait('document.querySelector(".live-input-copy small").textContent==="hello"')
            assert ''.join(evaluate('sentInputs()')) == 'hello'
            browser('press', 'Backspace')
            wait('document.querySelector(".live-input-copy small").textContent==="hell"')
            assert evaluate('sentInputs().at(-1)') == '\x7f'
            enter_before = evaluate('sentInputs().length')
            browser('press', 'Enter')
            wait('capture.value===""')
            assert evaluate('sentInputs().at(-1)') == '\r'
            assert evaluate('sentInputs().length') == enter_before + 1

            evaluate('globalThis.beforeIME=sentInputs().length;fire("compositionstart");edit("ㅎ",{isComposing:true});true')
            assert evaluate('document.querySelector(".live-input-copy small").textContent') == 'ㅎ'
            assert evaluate('sentInputs().length===beforeIME')
            assert evaluate('!document.querySelector(".terminal-preedit").hidden && document.querySelector(".terminal-preedit").getBoundingClientRect().width>0')
            evaluate('edit("한",{isComposing:true});fire("compositionend",{data:"한"});fire("input",{inputType:"insertFromComposition"});true')
            wait('sentInputs().at(-1)==="한"')
            assert evaluate('sentInputs().length===beforeIME+1')
            evaluate('edit("한국",{inputType:"insertText"});edit("한",{inputType:"deleteContentBackward"});true')
            assert evaluate('sentInputs().at(-1)') == '한'

            # A slow ACK used to reveal a new row under the terminal. Neither
            # ACK nor output/viewport updates may change its footprint or preview.
            for width, height in [(320, 740), (390, 844), (844, 390)]:
                browser('set', 'viewport', str(width), str(height))
                evaluate('visualViewport.height=innerHeight;visualViewport.dispatchEvent(new Event("resize"));true')
                evaluate(f'mountCase({width});true')
                wait('document.querySelector(".terminal-mount").dataset.phase==="ready"')
                browser('click', '.live-focus-target')
                wait('document.querySelector(".terminal-mount").dataset.keyboardOpen==="false" && parseInt(getComputedStyle(document.documentElement).getPropertyValue("--keyboard-inset")||"0")===0')
                evaluate('socket.autoAck=false;edit("한 live preview");true')
                initial = evaluate('document.querySelector(".terminal-frame").getBoundingClientRect().height')
                wait('document.querySelector(".terminal-mount").dataset.phase==="input-pending"')
                evaluate('socket.screen("output while ACK waits");true')
                pending = evaluate('document.querySelector(".terminal-frame").getBoundingClientRect().height')
                assert initial == pending, (width, initial, pending)
                assert evaluate('getComputedStyle(document.querySelector(".terminal-status")).display') == 'none'
                assert evaluate('document.querySelector(".live-input-copy small").textContent') == '한 live preview'
                tail_check = evaluate('''(()=>{
                  const frame=document.querySelector('.terminal-frame'),preedit=document.querySelector('.terminal-preedit');
                  const base=frame.getBoundingClientRect().height;
                  fire('compositionstart');
                  const text='한글 조합 중 받침과 자모 삭제를 브라우저가 처리하고 긴 미리보기의 끝부분까지 보입니다';
                  edit(capture.value+text,{isComposing:true,inputType:'insertCompositionText'});
                  const box=preedit.getBoundingClientRect(),parent=frame.getBoundingClientRect();
                  const result={ok:!preedit.hidden&&getComputedStyle(preedit).display!=='none'&&box.width>0&&box.left>=parent.left&&box.right<=parent.right&&box.bottom<=parent.bottom&&frame.getBoundingClientRect().height===base&&preedit.textContent===text,box:{left:box.left,right:box.right,bottom:box.bottom},parent:{left:parent.left,right:parent.right,bottom:parent.bottom}};
                  fire('compositionend',{data:text});fire('input',{inputType:'insertFromComposition'});
                  return result;
                })()''')
                assert tail_check.get('ok'), (width, tail_check)
                # Capture the visible native preedit as evidence, before commit.
                evaluate('fire("compositionstart");edit(capture.value+" 한글 조합 중",{isComposing:true});true')
                shot(f'{width}-preedit')
                evaluate('fire("compositionend",{data:"조합 중"});fire("input",{inputType:"insertFromComposition"});true')
                evaluate('edit("한 live preview",{inputType:"deleteContentBackward"});true')
                shot(f'{width}-pending')
                evaluate('for(const a of socket.sent.filter(a=>a instanceof Uint8Array&&a[4]===0x13)){const r=MkTerminalCodec.reader(a.slice(5));socket.frame(0x27,MkTerminalCodec.v(r.n()),MkTerminalCodec.v(0));}socket.autoAck=true;true')
                wait('document.querySelector(".terminal-mount").dataset.phase==="ready"')
                assert evaluate('document.querySelector(".terminal-frame").getBoundingClientRect().height') == initial
                evaluate('visualViewport.height=innerHeight-260;visualViewport.dispatchEvent(new Event("resize"));true')
                wait('document.querySelector(".terminal-mount").dataset.keyboardOpen==="true"')
                assert evaluate('document.querySelector(".live-input-copy small").textContent') == '한 live preview'
                assert evaluate('document.querySelector(".live-focus-target").getAttribute("aria-pressed")==="true"')
                assert evaluate('(()=>{const a=capture.getBoundingClientRect(),b=document.querySelector(".accessory-row").getBoundingClientRect();return a.top>=b.top&&a.bottom<=b.bottom;})()')
                shot(f'{width}-keyboard')
                browser('click', '.live-focus-target')
                assert evaluate('document.activeElement!==capture')
                evaluate('visualViewport.height=innerHeight;visualViewport.dispatchEvent(new Event("resize"));true')
                browser('click', '.live-focus-target')
                # The visual viewport is mocked here. Use a software-keyboard
                # event for cleanup; physical Chromium Enter is checked above.
                enter_before = evaluate('sentInputs().length')
                evaluate('fire("beforeinput",{inputType:"insertLineBreak"});true')
                assert evaluate('sentInputs().length') == enter_before + 1
                measurements.append({'width': width, 'height': height, 'frameBefore': initial,
                                     'framePending': pending, 'statusHidden': True})

            # Disconnect with an outstanding write, remount the same session,
            # and require explicit Resume without replaying either text or IME.
            evaluate('socket.autoAck=false;edit("do not replay");socket.close();true')
            wait('document.querySelector(".terminal-mount").dataset.phase==="disconnected"')
            evaluate('mountCase(844);true')
            wait('document.querySelector(".terminal-mount").dataset.phase==="ready"')
            assert evaluate('sentInputs()') == [], evaluate('sentInputs()')
            assert evaluate('document.querySelector(".live-input-copy small").dataset.preview') == 'false'
            assert evaluate('[...document.querySelectorAll("button")].some(n=>!n.hidden&&n.textContent==="Resume input")')
            browser('click', '.terminal-frame>button.ghost.small')
            assert evaluate('sentInputs()') == [], evaluate('sentInputs()')
            browser('click', '.direct-toggle')
            wait('document.querySelector(".terminal-mount").dataset.inputMode==="buffered"')
            browser('fill', '.command-input', 'once')
            evaluate('socket.autoAck=false;document.querySelector(".buffered-command").requestSubmit();document.querySelector(".buffered-command").requestSubmit();true')
            assert evaluate('sentInputs()') == ['once\r']
            evaluate('socket.frame(0x27,MkTerminalCodec.v(1),MkTerminalCodec.v(0));true')
            wait('document.querySelector(".command-input").value===""')
            (evidence / 'measurements.json').write_text(json.dumps(measurements, indent=2) + '\n')
            print('PASS Chromium: dock mode clicks, Orca key order/icon, English, Hangul composition, backspace, Enter, '
                  'stable delayed-ACK geometry, simulated keyboard, reconnect/no replay, duplicate submit')
        except Exception:
            state = evaluate('({inputs:sentInputs().slice(0,30),inputCount:sentInputs().length,events:inputEvents,capture:capture.value,active:document.activeElement?.className,phase:document.querySelector(".terminal-mount").dataset.phase,body:document.body.innerText})')
            (evidence / 'failure.json').write_text(json.dumps(state, ensure_ascii=False, indent=2) + '\n')
            shot('failure')
            raise
        finally:
            browser('close')
            server.shutdown()


if __name__ == '__main__':
    main()

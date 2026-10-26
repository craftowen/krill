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
            expected = ['escape', 'tab', 'enter', 'shiftTab', 'space', 'backspace',
                        'delete', 'arrowUp', 'arrowDown', 'arrowLeft', 'arrowRight']
            assert evaluate('[...document.querySelector(".keybar").children].slice(2,13).map(n=>n.dataset.keyId)') == expected
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
            evaluate('edit("한",{isComposing:true});fire("compositionend",{data:"한"});fire("input",{inputType:"insertFromComposition"});true')
            wait('sentInputs().at(-1)==="한"')
            assert evaluate('sentInputs().length===beforeIME+1')
            evaluate('edit("한국",{inputType:"insertText"});edit("한",{inputType:"deleteContentBackward"});true')
            assert evaluate('sentInputs().at(-1)') == '\x7f'

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
                  const small=document.querySelector(".live-input-copy small"),row=document.querySelector(".input-row"),live=document.querySelector(".live-focus-target"),frame=document.querySelector(".terminal-frame");
                  const snap=()=>({frameH:frame.getBoundingClientRect().height,rowH:row.getBoundingClientRect().height,rowTop:row.getBoundingClientRect().top,liveH:live.getBoundingClientRect().height,smallH:small.getBoundingClientRect().height});
                  const base=snap(),steps=[
                    "cargo nextest run -p krilld --locked",
                    "cargo nextest run -p krilld --locked -- 한글 긴 입력 미리보기에서도 방금 친 끝부분이",
                    "cargo nextest run -p krilld --locked --target-dir /Volumes/krill-build/targets/shared -- 한글 긴 입력 미리보기에서도 가로 모드를 포함해 방금 친 끝부분이 잘리지 않고 항상 보입니다 TAIL_OK"
                  ];
                  for(const text of steps){
                    edit(text,{inputType:"insertText"});
                    const now=snap();
                    if(now.frameH!==base.frameH||now.rowH!==base.rowH||now.rowTop!==base.rowTop||now.liveH!==base.liveH||now.smallH!==base.smallH)return {ok:false,reason:"flicker",base,now};
                  }
                  fire("compositionstart");
                  edit(steps[2]+" ㅎ",{isComposing:true});
                  edit(steps[2]+" 한",{isComposing:true});
                  const afterIme=snap();
                  if(afterIme.frameH!==base.frameH||afterIme.rowH!==base.rowH||afterIme.rowTop!==base.rowTop||afterIme.liveH!==base.liveH||afterIme.smallH!==base.smallH)return {ok:false,reason:"ime-flicker",base,afterIme};
                  const textNode=small.firstChild,box=small.getBoundingClientRect();
                  const tailRange=document.createRange();
                  tailRange.setStart(textNode,textNode.data.length-8);
                  tailRange.setEnd(textNode,textNode.data.length);
                  const tailRect=tailRange.getBoundingClientRect();
                  const headRange=document.createRange();
                  headRange.setStart(textNode,0);
                  headRange.setEnd(textNode,5);
                  const headRect=headRange.getBoundingClientRect();
                  fire("compositionend",{data:"한"});
                  fire("input",{inputType:"insertFromComposition"});
                  return {
                    ok:small.scrollWidth>small.clientWidth&&small.scrollLeft>0&&Math.abs(small.scrollLeft+small.clientWidth-small.scrollWidth)<=2&&tailRect.width>0&&tailRect.left>=box.left&&tailRect.right<=box.right+1.5&&headRect.left<box.left,
                    scrollWidth:small.scrollWidth,clientWidth:small.clientWidth,scrollLeft:small.scrollLeft,
                    box:{left:box.left,right:box.right},tail:{left:tailRect.left,right:tailRect.right,width:tailRect.width},head:{left:headRect.left,right:headRect.right}
                  };
                })()''')
                assert tail_check.get('ok'), (width, tail_check)
                evaluate('edit("한 live preview",{inputType:"deleteContentBackward"});true')
                shot(f'{width}-pending')
                evaluate('for(const a of socket.sent.filter(a=>a instanceof Uint8Array&&a[4]===0x13)){const r=MkTerminalCodec.reader(a.slice(5));socket.frame(0x27,MkTerminalCodec.v(r.n()),MkTerminalCodec.v(0));}socket.autoAck=true;true')
                wait('document.querySelector(".terminal-mount").dataset.phase==="ready"')
                assert evaluate('document.querySelector(".terminal-frame").getBoundingClientRect().height') == initial
                evaluate('visualViewport.height=innerHeight-260;visualViewport.dispatchEvent(new Event("resize"));true')
                wait('document.querySelector(".terminal-mount").dataset.keyboardOpen==="true"')
                assert evaluate('document.querySelector(".live-input-copy small").textContent') == '한 live preview'
                assert evaluate('!document.querySelector(".dismiss-keyboard").hidden')
                assert evaluate('(()=>{const a=capture.getBoundingClientRect(),b=document.querySelector(".input-row").getBoundingClientRect();return a.top>=b.top&&a.bottom<=b.bottom;})()')
                shot(f'{width}-keyboard')
                browser('click', '.dismiss-keyboard')
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
            print('PASS Chromium: Orca key order/icon, English, Hangul composition, backspace, Enter, '
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

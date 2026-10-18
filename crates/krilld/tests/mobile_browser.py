"""Optional real Chromium QA: build bins, then run with --address <LAN IPv4>:0.

Requires installed node + agent-browser; uses isolated HOME/state/browser and mock agents.
No account credentials, paid provider requests, or live user sessions are accessed.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import time


def until(check, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise AssertionError("browser fixture condition timed out")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", required=True)
    parser.add_argument("--screenshots", default="/tmp/m9-pwa-browser")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    screenshots = Path(args.screenshots).resolve()
    screenshots.mkdir(parents=True, exist_ok=True)
    session = f"m9-pwa-qa-{os.getpid()}"
    with tempfile.TemporaryDirectory(prefix="m9-browser-") as scratch:
        metadata = Path(scratch) / "fixture.json"
        env = dict(os.environ, M9_BROWSER_ADDRESS=args.address, M9_BROWSER_METADATA=str(metadata))
        with (Path(scratch) / "fixture.log").open("w+") as log:
            fixture = subprocess.Popen(
                ["node", str(Path(__file__).with_name("mobile_pwa.cjs")), str(target / "debug/krilld"), "--keep"],
                env=env, stdout=log, stderr=log,
            )
            try:
                until(metadata.exists)
                config = json.loads(metadata.read_text())

                def browser(*command):
                    result = subprocess.run(["agent-browser", "--session", session, *command], capture_output=True, text=True, timeout=40)
                    if result.returncode:
                        raise AssertionError(result.stdout + result.stderr)
                    return result.stdout

                def evaluate(script):
                    return json.loads(browser("eval", "--json", script))["data"]["result"]

                def wait(script):
                    browser("wait", "--fn", script)

                def read_session():
                    return subprocess.check_output(
                        [str(target / "debug/krill"), "read", str(config["session"])],
                        env=dict(os.environ, KRILL_STATE_DIR=config["dir"]), text=True,
                    )

                # Lose the connection after credentials arrive but before ServerHello.
                # This used to leave a registered device with a permanently empty screen.
                fault = Path(scratch) / "interrupt-once.js"
                fault.write_text("""
const NativeWebSocket=WebSocket;
globalThis.recoverySockets=[];
globalThis.WebSocket=class extends NativeWebSocket {
 constructor(...args){super(...args);recoverySockets.push(this);let received=0;
  this.addEventListener('message',e=>{
   if((++received===3&&!sessionStorage.getItem('interrupted'))||this.dropReply){
    this.dropReply=false;sessionStorage.setItem('interrupted','yes');
    e.stopImmediatePropagation();this.close();
   }
  });
 }
 send(data){if(this.dropAfterSend){this.dropAfterSend=false;this.dropReply=true;}return super.send(data);}
};
""")
                browser("--user-agent", "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
                        "--init-script", str(fault), "open", config["url"])
                browser("set", "viewport", "390", "844")
                wait('recoverySockets.length>=2 && document.querySelectorAll(".session-card").length===1 && document.getElementById("connection").textContent==="Encrypted · connected"')
                assert evaluate('sessionStorage.getItem("interrupted")') == "yes"
                assert len(json.loads((Path(config["dir"]) / "mobile-devices.json").read_text())) == 1
                assert evaluate('({secure:isSecureContext, subtle:typeof crypto.subtle, uuid:typeof crypto.randomUUID, hash:location.hash, paired:!!localStorage.getItem("minikrill-mobile-v2")})') == {
                    "secure": False, "subtle": "undefined", "uuid": "undefined", "hash": "", "paired": True,
                }
                assert not evaluate("document.documentElement.scrollWidth>innerWidth")
                browser("screenshot", str(screenshots / "sessions.png"))
                # A lost prompt reply must never cause a mutation to be replayed on reconnect.
                browser("fill", "#prompt", "mobile-reconnect-once")
                evaluate('recoverySockets.at(-1).dropAfterSend=true;document.getElementById("composer").requestSubmit();true')
                wait('recoverySockets.length>=3 && document.getElementById("connection").textContent==="Encrypted · connected"')
                assert read_session().count("got:mobile-reconnect-once") == 1
                assert evaluate('document.getElementById("prompt").value') == "mobile-reconnect-once"
                assert "not resent" in evaluate('document.getElementById("notice").textContent')
                browser("fill", "#prompt", "")
                terminal_button = f'[data-session="{config["session"]}"] button.primary'
                evaluate(f'document.querySelector({json.dumps(terminal_button)}).scrollIntoView({{block:"center"}});true')
                browser("click", terminal_button)
                wait('liveTerminal.ready && document.getElementById("terminal-grid").getAttribute("aria-label").includes("READY")')
                browser("screenshot", str(screenshots / "terminal-after-reconnect.png"))
                evaluate('liveTerminal.close();document.getElementById("terminal-panel").hidden=true;scrollTo(0,0);true')
                browser("scroll", "down", "450")
                browser("screenshot", str(screenshots / "sessions-detail.png"))
                browser("scroll", "up", "450")
                browser("find", "role", "button", "click", "--name", "Approve (Yes)", "--exact")
                until(lambda: "got:y" in read_session())
                browser("fill", "#prompt", "한글 조합 테스트")
                evaluate('document.getElementById("prompt").dispatchEvent(new CompositionEvent("compositionstart"));document.getElementById("composer").requestSubmit();true')
                assert "got:한글 조합 테스트" not in read_session()
                evaluate('document.getElementById("prompt").dispatchEvent(new CompositionEvent("compositionend"));true')
                browser("click", "#send")
                until(lambda: "got:한글 조합 테스트" in read_session())
                browser("find", "role", "button", "click", "--name", "Approve", "--exact")
                wait('!Array.from(document.querySelectorAll("#urgent-items strong")).some(e=>e.textContent==="Deploy fixture?")')
                browser("find", "role", "button", "click", "--name", "Orchestration", "--exact")
                browser("fill", 'input[placeholder="Reply to worker"]', "모바일 응답")
                evaluate('document.querySelector("#orchestration input").focus();true')
                assert evaluate('new Promise(r=>setTimeout(()=>r(document.querySelector("#orchestration input")?.value),3100))') == "모바일 응답"
                browser("screenshot", str(screenshots / "orchestration.png"))
                browser("find", "role", "button", "click", "--name", "Send reply", "--exact")
                wait('!document.querySelector("#orchestration input")')
                store = json.loads((Path(config["dir"]) / "orchestration.json").read_text())
                assert any(m["body"] == "모바일 응답" and m.get("reply_to") for m in store["messages"])
                browser("find", "role", "button", "click", "--name", "Accounts", "--exact")
                wait('Array.from(document.querySelectorAll(".account-card h2")).some(e=>e.textContent==="Mobile fixture")')
                fixture_account = 'Array.from(document.querySelectorAll(".account-card")).find(e=>e.querySelector("h2").textContent==="Mobile fixture")'
                assert evaluate(f'Array.from(({fixture_account}).querySelectorAll("progress")).map(e=>e.value)') == [23, 47]
                browser("find", "role", "button", "click", "--name", "Switch to this account", "--exact")
                wait(f'!!({fixture_account}).querySelector(".active-account")')
                browser("scroll", "down", "1200")
                browser("screenshot", str(screenshots / "accounts.png"))
                browser("find", "role", "button", "click", "--name", "Sessions", "--exact")
                photo = Path(scratch) / "photo.png"
                photo.write_bytes(base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a9sAAAAASUVORK5CYII="))
                browser("upload", "#photo", str(photo))
                wait('document.getElementById("attachment-status").textContent.includes("attached")')
                uploads = list((Path(config["dir"]) / "paste/minikrill-paste").glob("mobile-*.jpg"))
                assert len(uploads) == 1 and stat.S_IMODE(uploads[0].stat().st_mode) == 0o600
                browser("scroll", "down", "500")
                browser("find", "role", "button", "click", "--name", "Git diff", "--exact")
                wait('document.getElementById("git-dialog").open')
                browser("find", "role", "button", "click", "--name", "example.txt", "--exact")
                wait('document.getElementById("git-patch").textContent.includes("+after")')
                browser("screenshot", str(screenshots / "diff.png"))
                browser("click", "#close-git")
                browser("find", "role", "button", "click", "--name", "→ Claude", "--exact")
                wait('document.querySelectorAll(".session-card").length===2')
                browser("reload")
                wait('document.getElementById("connection").textContent==="Encrypted · connected"')
                assert evaluate('location.hash') == ""
                assert not evaluate('document.getElementById("notice").textContent')
                assert not browser("errors").strip()
                print("Chromium LAN HTTP QA passed: interrupted pairing recovery, no prompt replay, live terminal, non-secure E2EE, approval, IME, gate/question, accounts, photo, diff, mock handoff, reconnect")
                print(f"Screenshots: {screenshots}")
            finally:
                subprocess.run(["agent-browser", "--session", session, "close"], capture_output=True, timeout=15)
                fixture.terminate()
                try:
                    fixture.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait()
                if fixture.returncode not in (0, -15):
                    log.seek(0)
                    print(log.read())


if __name__ == "__main__":
    main()

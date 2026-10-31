var issued=0, inputAttempted=false, mayHaveEffect=false, cleanup="not_needed", clipboard=null;
var proc=null, win=null, el=null, refs=[];
function result(ok,status,reason,note,delivery) {
  const state=delivery || (issued ? "submitted" : "not_dispatched");
  return {ok,status,reason,note,delivery:state,issuedUnits:state==="unknown"?null:issued,cleanup,clipboard};
}
function guard(proc,win) {
  if(processStart()!==req.processStart) throw Error("process start witness changed");
  checkCGWindow(false);foreground(proc,win);
  // Recheck enabled/ancestor values before every unit. Only a text write in this
  // request may change its own target value; keys and unrelated ancestors cannot.
  checkTargetsLive(refs,issued>0 && ["setValue","typeText","pasteText"].indexOf(String(req.kind))>=0);
  if(["pressKey","hotkey","typeText","pasteText"].indexOf(String(req.kind))>=0) {
    if(req.targetRef && el) {
      if(!Boolean(el.focused())) throw Error("text receiver focus changed");
    } else {
      if(!req.receiver) throw Error("keyboard receiver was not observed");
      const receiver=proc.attributes.byName("AXFocusedUIElement").value();
      if(!receiver || String(receiver.role())!==req.receiver.role || !equal(rect(receiver),req.receiver.bounds)) throw Error("keyboard receiver changed");
      let title=null;try {title=receiver.title();} catch(_) {}
      if((title||null)!==(req.receiver.title||null)) throw Error("keyboard receiver label changed");
    }
  }
  if(["pressKey","hotkey","typeText","pasteText"].indexOf(String(req.kind))>=0) {
    // This public symbol is absent from some JXA BridgeSupport metadata.
    if(typeof $.IsSecureEventInputEnabled !== "function") {
      try {ObjC.bindFunction("IsSecureEventInputEnabled",["bool",[]]);}
      catch(_) {throw Error("Secure Event Input status unavailable");}
    }
    if(typeof $.IsSecureEventInputEnabled !== "function") throw Error("Secure Event Input status unavailable");
    if($.IsSecureEventInputEnabled()) throw Error("Secure Event Input is active");
  }
}
function issue(f) {inputAttempted=true;mayHaveEffect=true;f();issued++;}
function post(event) {if(!event) throw Error("event allocation failed");issue(()=>$.CGEventPost(0,event));}
function assertPoint(pt) {
  const b=req.winBounds;
  if(!pt || !Number.isFinite(pt.x) || !Number.isFinite(pt.y) || pt.x<b.x || pt.y<b.y || pt.x>b.x+b.width || pt.y>b.y+b.height) throw Error("point outside observed window");
  // CG's front-to-back list proves the pixel receiver belongs to the selected window.
  const raw=$.CGWindowListCopyWindowInfo(1,0), rows=ObjC.deepUnwrap(ObjC.castRefToObject(raw));
  const hit=rows.find(w=> {const z=w.kCGWindowBounds;return z && Number(w.kCGWindowLayer||0)===0 && pt.x>=z.X && pt.y>=z.Y && pt.x<=z.X+z.Width && pt.y<=z.Y+z.Height;});
  if(!hit || String(hit.kCGWindowNumber)!==String(req.winId)) throw Error("pixel receiver is occluded or changed");
  // A layer-0 window census alone misses overlays, sheets and menus. Require the
  // actual AX hit to belong to the focused window already checked against our snapshot.
  const system=$.AXUIElementCreateSystemWide(), actual=Ref(), owner=Ref(), focused=Ref();
  if(Number($.AXUIElementCopyElementAtPosition(system,pt.x,pt.y,actual))!==0 ||
     Number($.AXUIElementGetPid(actual[0],owner))!==0 || Number(owner[0])!==Number(req.pid)) {
    throw Error("pixel receiver hit-test is unavailable or belongs to another application");
  }
  const app=$.AXUIElementCreateApplication(Number(req.pid));
  if(Number($.AXUIElementCopyAttributeValue(app,$("AXFocusedWindow"),focused))!==0) throw Error("focused AX window unavailable");
  let cursor=actual[0], belongs=false;
  for(let depth=0;depth<64;depth++) {
    if($.CFEqual(cursor,focused[0])) {belongs=true;break;}
    const parent=Ref();
    if(Number($.AXUIElementCopyAttributeValue(cursor,$("AXParent"),parent))!==0) break;
    if($.CFEqual(cursor,parent[0])) break;
    cursor=parent[0];
  }
  if(!belongs) throw Error("pixel receiver is outside the observed focused window");
}
function mouse(pt,button,count) {
  assertPoint(pt);
  const n=button==="right" ? 1 : button==="middle" ? 2 : 0;
  const down=n===1?3:n===2?25:1, up=n===1?4:n===2?26:2;
  for(let i=1;i<=count;i++) {
    guard(proc,win);assertPoint(pt);
    const d=$.CGEventCreateMouseEvent(null,down,pt,n),u=$.CGEventCreateMouseEvent(null,up,pt,n);
    if(!d||!u) throw Error("mouse pair allocation failed");
    $.CGEventSetIntegerValueField(d,1,i);$.CGEventSetIntegerValueField(u,1,i);
    $.CGEventSetFlags(d,Number(req.cgFlags||0));$.CGEventSetFlags(u,Number(req.cgFlags||0));
    try {post(d);} finally {post(u);}
  }
}
function utf16Boundary(text,index) {
  if(index<=0 || index>=text.length) return true;
  return !((text.charCodeAt(index)&0xFC00)===0xDC00 && (text.charCodeAt(index-1)&0xFC00)===0xD800);
}
// The receiver's selection, decoded only when exactly one span encoding reproduces
// AXSelectedText on UTF-16 boundaries. System Events reported [1,8] for all of
// "baseline" (1-based inclusive); the 0-based location/length form is also tried so
// an unproven encoding is refused rather than guessed.
function readSelection(target,before) {
  let range=null, selected=null;
  try { range=target.attributes.byName("AXSelectedTextRange").value(); } catch(_) {}
  try { selected=target.attributes.byName("AXSelectedText").value(); } catch(_) {}
  if(!Array.isArray(range) || range.length!==2 || typeof selected!=="string") throw Error("receiver selection unavailable");
  const a=Number(range[0]), b=Number(range[1]);
  if(!Number.isInteger(a) || !Number.isInteger(b)) throw Error("receiver selection unavailable");
  const spans=[[a-1,b],[a,a+b]].filter(([s,e])=>s>=0 && e>=s && e<=before.length &&
    before.slice(s,e)===selected && utf16Boundary(before,s) && utf16Boundary(before,e));
  if(spans.length!==1) throw Error("receiver selection ambiguous");
  return {start:spans[0][0],end:spans[0][1],raw:[a,b],selected};
}
function sameSelection(x,y) {return x.start===y.start && x.end===y.end && x.selected===y.selected;}
function receiverState(target) {
  const value=liveValue(target);
  if(value===null) throw Error("receiver full value unavailable");
  return {value,selection:readSelection(target,value)};
}
function restoreClipboard(pb,saved,owned) {
  // Best effort, not compare-and-swap: another writer can still land between this
  // changeCount check and clearContents. A conflict seen here retains the user's data.
  if(Number(pb.changeCount)!==owned) throw Error("clipboard conflict before restoration; user contents retained");
  pb.clearContents;
  if(saved.length && !pb.writeObjects($(saved))) throw Error("clipboard restoration failed");
  clipboard.restored=true;cleanup="completed";
}
function run(argv) {
  req=parseRequest(argv);
  try {
    // Fallback from a native snapshot for an action without element targets: nothing to find in a tree,
    // and the worker reads the process start through another API, measured 6 ms apart for one process.
    if(req.noTree) {
      const start=processStart();
      if(Math.abs(Number(start)-Number(req.processStart))>=1) throw Error("process instance changed");
      req.processStart=start;
    }
    if(!req.processStart || processStart()!==req.processStart) throw Error("process instance changed");
    checkCGWindow(false);proc=getProcess();win=selectTargetWindow(proc);
    if(!req.noTree) refs=validateTree(readNode(win,0,true));
    el=req.targetRef ? entries[Number(req.targetRef)] : null;
    guard(proc,win);
    const kind=String(req.kind||"");
    if(kind==="click" || kind==="secondaryAction") {
      const simple=kind==="secondaryAction" || ((!req.button || req.button==="left") && Number(req.clickCount||1)===1 && (!req.modifiers||!req.modifiers.length));
      if(el && simple) {
        const name=kind==="secondaryAction" ? String(req.actionName||"AXShowMenu") : "AXPress";
        if(el.actions.name().indexOf(name)<0) throw Error("requested AX action unavailable; no coordinate fallback");
        issue(()=>el.actions.byName(name).perform());
      } else {
        if(kind==="secondaryAction") throw Error("secondary action requires original reference");
        mouse(req.point,req.button||"left",Math.min(16,Number(req.clickCount||1)));
      }
      return JSON.stringify(result(true,"unverified","action submitted; no business predicate requested","single dispatch; no fallback"));
    }
    if(kind==="setValue") {
      if(!el) throw Error("set-value requires original reference");
      issue(()=>{el.attributes.byName("AXValue").value=String(req.value);});
      // Full readback on the SAME resolved object; never use the 500-character preview.
      guard(proc,win);
      const actual=liveValue(el);
      if(actual!==String(req.value)) return JSON.stringify(result(false,"unverified","full target value readback differs",null));
      return JSON.stringify(result(true,"verified",null,"full AXValue equality on resolved target"));
    }
    if(kind==="typeText" || kind==="pasteText") {
      if(!el) throw Error("text input requires an observed editable receiver");
      const role=String(el.role());
      if(["AXTextField","AXTextArea","AXComboBox"].indexOf(role)<0) throw Error("receiver is not an editable text control");
      if(!Boolean(el.focused())) throw Error("text receiver focus changed; observe or explicitly focus first");
      if(kind==="typeText") {
        // One System Events dispatch, no newline/tab sub-actions with unchecked receiver changes.
        if(/[\r\n\t]/.test(String(req.text))) throw Error("multiline/tab input requires set-value or paste-text");
        issue(()=>se.keystroke(String(req.text||"")));
        return JSON.stringify(result(true,"unverified","keystroke submitted; inserted text not asserted",null));
      }
      const text=String(req.text||"");
      // The exact full value this paste must produce, fixed before any clipboard write.
      const before=receiverState(el);
      const expected=before.value.slice(0,before.selection.start)+text+before.value.slice(before.selection.end);
      if(expected===before.value) throw Error("paste would not change the receiver; consumption unprovable");
      if(expected.length>65536) throw Error("expected receiver value exceeds bounded full readback");
      const pb=$.NSPasteboard.generalPasteboard;
      const initial=Number(pb.changeCount);
      const items=pb.pasteboardItems, saved=[];
      let size=0;
      for(let i=0;i<Number(items.count);i++) {
        const item=items.objectAtIndex(i), copy=$.NSPasteboardItem.alloc.init, types=item.types;
        for(let j=0;j<Number(types.count);j++) {
          const type=types.objectAtIndex(j), data=item.dataForType(type);
          if(!data || data.isNil()) throw Error("clipboard promised type cannot be preserved");
          size+=Number(data.length);
          if(size>4194304) throw Error("clipboard preservation exceeds bound; unchanged");
          if(!copy.setDataForType(data,type)) throw Error("clipboard type copy failed");
        }
        saved.push(copy);
      }
      if(Number(pb.changeCount)!==initial) throw Error("clipboard changed during preservation");
      guard(proc,win);
      const current=receiverState(el);
      if(current.value!==before.value || !sameSelection(current.selection,before.selection)) throw Error("receiver value or selection changed before clipboard write");
      clipboard={preserved:true,restored:false};cleanup="incomplete";mayHaveEffect=true;
      pb.clearContents;
      if(!pb.setStringForType(text,$.NSPasteboardTypeString)) throw Error("clipboard write failed");
      const owned=Number(pb.changeCount);
      try {
        guard(proc,win);
        if(Number(pb.changeCount)!==owned || !Boolean(el.focused())) throw Error("clipboard or receiver changed before paste");
        const last=receiverState(el);
        if(last.value!==before.value || !sameSelection(last.selection,before.selection)) throw Error("receiver value or selection changed before paste");
        issue(()=>se.keystroke("v",{using:"command down"}));
        const deadline=Date.now()+750;
        let consumed=false;
        while(Date.now()<deadline) {
          if(Number(pb.changeCount)!==owned) throw Error("clipboard conflict; user contents retained");
          guard(proc,win);
          // Exact expected full value only; a looser "contains" can match an unrelated edit
          // and would restore the old clipboard before a delayed receiver reads it.
          if(liveValue(el)===expected) {consumed=true;break;}
          $.NSThread.sleepForTimeInterval(0.025);
        }
        if(!consumed) throw Error("paste consumption unconfirmed; clipboard retained for delayed receiver");
        restoreClipboard(pb,saved,owned);
        return JSON.stringify(result(true,"verified",null,"receiver full value equals the expected pasted value"));
      } catch(error) {
        let reason=String(error);
        if(!inputAttempted) {
          // Nothing consumed the temporary clipboard yet; put the user's contents back.
          try { restoreClipboard(pb,saved,owned); } catch(restoreError) { reason+="; "+String(restoreError); }
        }
        return JSON.stringify(result(false,"unverified",reason,null,issued?"submitted":inputAttempted?"unknown":clipboard.restored?"not_dispatched":"unknown"));
      }
    }
    if(kind==="pressKey" || kind==="hotkey") {
      const opts=Array.isArray(req.usingMods)&&req.usingMods.length ? {using:req.usingMods} : null;
      for(let i=0;i<Math.min(32,Math.max(1,Number(req.repeat||1)));i++) {
        guard(proc,win);
        if(req.keyCode!==null && req.keyCode!==undefined) issue(()=>opts ? se.keyCode(Number(req.keyCode),opts) : se.keyCode(Number(req.keyCode)));
        else issue(()=>opts ? se.keystroke(String(req.key),opts) : se.keystroke(String(req.key)));
      }
      return JSON.stringify(result(true,"unverified","key events submitted; no predicate requested",null));
    }
    if(kind==="scroll") {
      assertPoint(req.point);guard(proc,win);
      const move=$.CGEventCreateMouseEvent(null,5,req.point,0);
      const scroll=$.CGEventCreateScrollWheelEvent(null,0,2,Math.round(req.deltaY||0),Math.round(req.deltaX||0));
      if(!move||!scroll) throw Error("scroll event allocation failed");
      post(move);guard(proc,win);post(scroll);
      return JSON.stringify(result(true,"unverified","scroll submitted; no predicate requested",null));
    }
    if(kind==="drag") {
      assertPoint(req.from);assertPoint(req.to);
      const down=$.CGEventCreateMouseEvent(null,1,req.from,0), up=$.CGEventCreateMouseEvent(null,2,req.to,0);
      if(!down||!up) throw Error("drag pair allocation failed");
      cleanup="incomplete";
      try {
        post(down);
        for(let i=1;i<=5;i++) {
          guard(proc,win);
          const point={x:req.from.x+(req.to.x-req.from.x)*i/5,y:req.from.y+(req.to.y-req.from.y)*i/5};
          assertPoint(point);post($.CGEventCreateMouseEvent(null,6,point,0));
          $.NSThread.sleepForTimeInterval(0.01);
        }
      } finally {post(up);cleanup="completed";}
      return JSON.stringify(result(true,"unverified","drag submitted; no predicate requested",null));
    }
    throw Error("unsupported action");
  } catch(error) {
    return JSON.stringify(result(false,"unverified",String(error),"no mutation replay or backend fallback",mayHaveEffect?"unknown":"not_dispatched"));
  }
}

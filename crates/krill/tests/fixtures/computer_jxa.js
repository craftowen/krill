// Synthetic bridge regressions for the actual production scripts. All AppKit,
// accessibility, input and pasteboard calls below use fakes. Only Foundation file
// reads use the host. Run through macOS JavaScriptCore with osascript, no Node.
function run(argv) {
  ObjC.import("Foundation");
  const read=p=>{const s=$.NSString.stringWithContentsOfFileEncodingError(p,$.NSUTF8StringEncoding,null);if(!s || s.isNil())throw Error("fixture source unavailable");return ObjC.unwrap(s);};
  const dir=String(argv[0]);
  const common=read(dir+"/common.js"),observeScript=common+read(dir+"/../capture/geometry.js")+read(dir+"/observe.js"),actionScript=common+read(dir+"/action.js");
  const console={log:()=>{}};
  const assert={
    equal:(a,b,message)=>{if(a!==b)throw Error(String(message||"equality")+": "+JSON.stringify(a)+" != "+JSON.stringify(b));},
    deepEqual:(a,b,message)=>{if(JSON.stringify(a)!==JSON.stringify(b))throw Error(String(message||"deep equality")+": "+JSON.stringify(a)+" != "+JSON.stringify(b));},
    match:(value,pattern)=>{if(!pattern.test(String(value)))throw Error("pattern "+pattern+" did not match "+value);}
  };
  const vm={createContext:context=>context,runInContext:(script,context)=>{
    const factory=new Function("ObjC","$","Ref","Application","Date","console",script+"\nreturn run;");
    context.run=factory(context.ObjC,context.$,context.Ref,context.Application,context.Date,context.console);
  }};
function runCase(mode) {
  const effects=[], bounds={x:0,y:0,width:400,height:300};
  const state={value:'before',sel:[7,6],selText:'',clipboard:'original',change:1,pbWrites:0,writes:0,frontPid:7777,visible:true,minimized:false};
  const shape=[]; // element factory
  function attrObj(get,set) {const a={value:()=>get()};Object.defineProperty(a,'value',{get:()=>()=>get(),set:v=>set(v)});return a;}
  function element(role,title,b,opts={}) {
    const e={kind:role,enabledFlag:opts.enabled!==false,role:()=>role,title:()=>title,name:()=>title,subrole:()=>null,description:()=>null,
      position:()=>[b.x,b.y],size:()=>[b.width,b.height],enabled:()=>e.enabledFlag,focused:()=>role==='AXTextField',
      value:()=>opts.value?state.value:null};
    e.uiElements={role:()=>[]};
    e.actions={name:()=>role==='AXButton'?['AXPress']:[],byName:n=>({perform:()=>{effects.push(n);if(mode==='press-throws-after-effect')throw Error('reply lost after effect');}})};
    const attrs={
      AXValue:attrObj(()=>state.value,v=>{state.writes++;state.value=mode==='setvalue-normalized'?String(v)+'!':v;if(mode==='write-throws-after-effect')throw Error('AX write reply lost');}),
      AXSelectedTextRange:attrObj(()=>mode==='paste-selection-unavailable'?undefined:state.sel,()=>{throw Error('ro');}),
      AXSelectedText:attrObj(()=>mode==='paste-selection-unavailable'?undefined:state.selText,()=>{throw Error('ro');}),
    };
    if(mode==='metadata-settable-true' || mode==='metadata-settable-false') attrs.AXValue.settable=()=>mode==='metadata-settable-true';
    e.attributes={byName:n=>{if(!attrs[n]) throw Error('no attribute '+n);return attrs[n];}};
    return e;
  }
  function windowEl(title,number) {
    const w=element('AXWindow',title,bounds);
    w.actions={name:()=>['AXRaise'],byName:n=>({perform:()=>{effects.push('AXRaise');if(mode!=='restore-focus-other')focusedWin=w;}})};
    const attrs={AXWindowNumber:attrObj(()=>{if(number===null)throw Error('no AXWindowNumber');return number;},()=>{throw Error('ro');}),
      AXMinimized:attrObj(()=>state.minimized,v=>{effects.push('unminimize');state.minimized=Boolean(v);})};
    w.attributes={byName:n=>{if(!attrs[n]) throw Error('no attribute '+n);return attrs[n];}};
    return w;
  }
  const text=element('AXTextField','Input',{x:10,y:20,width:200,height:30},{value:true,enabled:mode!=='disabled-target'});
  const button=element('AXButton','Save',{x:220,y:20,width:60,height:30});
  let children=[text,button];
  if(mode==='incomplete-census') children=[text,button,...Array.from({length:38},(_,i)=>element('AXGroup','Filler'+i,bounds)),element('AXButton','Save',{x:220,y:20,width:60,height:30})];
  // Partial census away from the target: a depth-limited subtree elsewhere in the window.
  const setKids=(e,kids)=>{e.uiElements={role:()=>kids.map(k=>k.role()),...Object.fromEntries(kids.map((k,i)=>[i,k]))};};
  const deepPanel=()=>{const panel=element('AXGroup','Panel',bounds);let node=panel;for(let d=2;d<=4;d++){const next=element('AXGroup','Deep'+d,bounds);setKids(node,[next]);node=next;}setKids(node,[element('AXGroup','TooDeep',bounds)]);return panel;};
  if(mode==='partial-census-elsewhere') children=[text,button,deepPanel()];
  if(mode==='partial-census-duplicate') children=[text,button,element('AXButton','Save',{x:220,y:20,width:60,height:30}),deepPanel()];
  if(mode==='partial-census-unreadable-bounds'){button.position=()=>{throw Error('no position');};children=[text,button,deepPanel()];}
  if(mode==='partial-census-ancestor-cut'){const rows=element('AXGroup','Rows',bounds);setKids(rows,[text,button,...Array.from({length:40},(_,i)=>element('AXGroup','Filler'+i,bounds))]);children=[rows];}
  const noNumber=['same-geometry-no-number','geometry-unique-no-number'].includes(mode);
  const win=windowEl('Fixture',noNumber?null:101);
  const collection=()=>({role:()=>children.map(e=>e.role()),...Object.fromEntries(children.map((e,i)=>[i,e]))});
  win.uiElements=collection();
  if(mode==='ancestor-reused') {const row=element('AXRow','Account',bounds,{value:true});row.uiElements=collection();win.uiElements={role:()=>['AXRow'],0:row};}
  let windows=[win];
  if(mode==='focused-other-number' || mode==='restore-focus-other') windows=[win,windowEl('Fixture',202)];
  let focusedWin=mode==='focused-other-number'?windows[1]:win;
  if(mode==='restore-focus-other') focusedWin=windows[1];
  const winCollection=()=>({name:()=>windows.map(w=>w.title()),...Object.fromEntries(windows.map((w,i)=>[i,w]))});
  const procAttrs={AXFocusedWindow:{value:()=>focusedWin},AXFocusedUIElement:{value:()=>text}};
  const proc={unixId:()=>7777,windows:winCollection(),attributes:{byName:n=>procAttrs[n]}};
  Object.defineProperty(proc,'frontmost',{get:()=>()=>state.frontPid===7777,set:v=>{effects.push('frontmost');if(v && mode!=='restore-postcondition-fails')state.frontPid=7777;}});
  Object.defineProperty(proc,'visible',{get:()=>()=>state.visible,set:v=>{effects.push('visible');state.visible=Boolean(v);}});
  const se={processes:{whose:()=>({first:proc})},keystroke:(s,opts)=>{effects.push('key:'+s);
    if(s==='v' && opts) {
      if(mode==='clipboard-conflict'){state.clipboard='user copied text';state.change++;return;}
      if(mode==='paste-unconfirmed') return;
      if(mode==='paste-contains-unrelated'){state.value+=' unrelated-user-change';return;}
      const [s0,e0]=[state.sel[0]-1,state.sel[1]];
      state.value=state.value.slice(0,s0)+state.clipboard+state.value.slice(e0)+(mode==='paste-extra-suffix'?' zzz':'');if(mode==='paste-throws-after-effect') throw Error('paste reply lost after effect');
    }},keyCode:()=>{effects.push('keyCode');if(mode==='disable-after-first-key') text.enabledFlag=false;}};
  const cgRow=()=>({kCGWindowNumber:101,kCGWindowOwnerPID:7777,kCGWindowBounds:{X:0,Y:0,Width:400,Height:300},kCGWindowLayer:0});
  const launchDate={isNil:()=>false,timeIntervalSince1970:123.456789};
  const running={isNil:()=>false,processIdentifier:7777,launchDate,bundleIdentifier:null,executableURL:{lastPathComponent:'fixture'}};
  const data={isNil:()=>false,length:mode==='clipboard-too-large'?5000000:8};
  const pb={get changeCount(){return state.change;},pasteboardItems:{count:1,objectAtIndex:()=>({types:{count:1,objectAtIndex:()=> 'text'},dataForType:()=>data})},
    setStringForType:s=>{state.pbWrites++;state.clipboard=s;state.change++;if(mode==='paste-selection-changed'){state.sel=[1,6];state.selText='before';}return mode!=='clipboard-write-fails';},
    writeObjects:()=>{state.pbWrites++;state.clipboard='original';state.change++;return true;}};
  Object.defineProperty(pb,'clearContents',{get:()=>{state.pbWrites++;state.clipboard='';state.change++;return state.change;}});
  const dollar=items=>items;
  Object.assign(dollar,{NSRunningApplication:{runningApplicationWithProcessIdentifier:()=>running},NSProcessInfo:{processInfo:{processIdentifier:999}},
    NSWorkspace:{sharedWorkspace:{get frontmostApplication(){return {processIdentifier:mode==='foreground-changed'?8888:state.frontPid};}}},
    CGWindowListCopyWindowInfo:option=>option===1 && (state.minimized||!state.visible)?[]:[cgRow()],
    CGEventCreateMouseEvent:()=>({}),CGEventSetIntegerValueField:()=>{},CGEventSetFlags:()=>{},CGEventPost:()=>effects.push('CGEvent'),
    AXUIElementCreateSystemWide:()=>'system',AXUIElementCreateApplication:()=>'app',
    AXUIElementCopyElementAtPosition:(s,x,y,out)=>{out[0]='winRef';return 0;},AXUIElementGetPid:(e,out)=>{out[0]=7777;return 0;},
    AXUIElementCopyAttributeValue:(e,name,out)=>{if(name==='AXFocusedWindow'){out[0]='winRef';return 0;}return -1;},CFEqual:(a,b)=>a===b,
    NSPasteboard:{generalPasteboard:pb},NSPasteboardTypeString:'text',NSPasteboardItem:{alloc:{get init(){return {setDataForType:()=>true};}}},
    NSThread:{sleepForTimeInterval:()=>{}},IsSecureEventInputEnabled:()=>false});
  if(mode.startsWith('secure-')) dollar.IsSecureEventInputEnabled=undefined;
  let tick=0;
  function sandbox() {return vm.createContext({ObjC:{bindFunction:()=>{if(mode==='secure-unavailable') throw Error('binding unavailable');if(mode!=='secure-binding-noop') dollar.IsSecureEventInputEnabled=()=>mode==='secure-bound-active';},import:()=>{},unwrap:v=>v,deepUnwrap:v=>v,castRefToObject:v=>v},$:dollar,Ref:()=>[],Application:()=>se,Date:{now:()=>{tick+=50;return tick;}},console});}
  const request={pid:7777,processStart:'123.456789',winId:'101',winTitle:'Fixture',winBounds:{...bounds},kind:'click',targetRef:2,button:'left',clickCount:1,modifiers:[],point:{x:250,y:35}};
  // Base tree is read by the SAME production observation script before adversarial changes.
  const observer=sandbox();vm.runInContext(observeScript,observer);
  if(mode.startsWith('restore')) {
    state.visible=false;state.minimized=true;state.frontPid=4242;request.restore=true;
    let out=null,err=null;
    try {out=JSON.parse(observer.run([JSON.stringify(request)]));} catch(e) {err=String(e);}
    const once=k=>effects.filter(x=>x===k).length;
    if(mode==='restore-normal'){assert.equal(err,null,err);assert.equal(out.root.role,'AXWindow');assert.deepEqual(effects,['visible','unminimize','frontmost','AXRaise']);}
    else {assert.match(err,/restore outcome unknown; .*no fallback/);for(const k of ['visible','unminimize','frontmost','AXRaise']) assert.equal(once(k),1,mode+' '+k);}
    return {mode,result:out?{rootRole:out.root.role}:{error:err},effects};
  }
  const observed=JSON.parse(observer.run([JSON.stringify(request)]));
  if(mode.startsWith('partial-census')) assert.equal(observed.truncated,true,mode);
  if(mode.startsWith('metadata-settable-')) {
    const expected=mode==='metadata-settable-unavailable'?null:mode==='metadata-settable-true';
    assert.equal(observed.root.children[0].valueSettable,expected);
    assert.equal(observed.root.valueSettable,null);
    assert.deepEqual(effects,[]);assert.equal(state.writes,0);
    return {mode,result:{valueSettable:observed.root.children[0].valueSettable},effects};
  }
  request.baseRoot=observed.root;
  if(mode==='ancestor-reused'){request.targetRef=3;state.value='different semantic account';}
  if(mode==='disable-after-first-key'){request.kind='pressKey';request.targetRef=1;request.repeat=2;request.keyCode=36;}
  if(mode.startsWith('secure-')){request.kind='typeText';request.targetRef=1;request.text='x';}
  if(mode==='focused-other-number'){focusedWin=windows[1];}
  if(mode==='same-geometry-no-number'){windows=[win,windowEl('Fixture',null)];proc.windows=winCollection();focusedWin=windows[1];}
  if(mode==='reorder'){children.reverse();win.uiElements=collection();}
  if(mode==='ambiguous'){children.push(text);win.uiElements=collection();}
  if(mode==='geometry-same-winid-different'){windows=[windowEl('Fixture',303)];windows[0].uiElements=collection();proc.windows=winCollection();focusedWin=windows[0];}
  if(mode==='disabled-target'){request.kind='setValue';request.targetRef=1;request.value='x';}
  if(mode.startsWith('write')||mode==='long-full-value'||mode==='setvalue-normalized'){request.kind='setValue';request.targetRef=1;request.value='가'.repeat(1200)+'tail';}
  if(mode.startsWith('clipboard')||mode.startsWith('paste')){request.kind='pasteText';request.targetRef=1;request.text=' pasted';}
  if(mode==='pid-reused')request.processStart='123.456790';
  // Regression for the v1 false positive: a REAL setValue-capable target whose value changed.
  if(mode==='value-reused'){request.kind='setValue';request.targetRef=1;request.value='OVERWRITE';state.value='different semantic account';}
  if(mode==='press-unavailable-no-fallback'){request.targetRef=1;}
  if(mode.startsWith('drag')){request.kind='drag';delete request.targetRef;request.targetRefs=[1,2];request.from={x:20,y:30};request.to={x:240,y:30};}
  if(mode==='drag-target-value-changed') state.value='different semantic account';
  if(mode==='drag-target-disabled') text.enabledFlag=false;
  if(mode==='paste-contains-unrelated'){state.value='before pasted';state.sel=[14,13];}
  if(mode==='paste-long-selection'){state.value='가'.repeat(1000)+'tail';state.sel=[1001,1004];state.selText='tail';}
  if(mode==='paste-selection-ambiguous'){state.sel=[1,0];}
  if(mode==='paste-noop'){state.value='before pasted';state.sel=[7,13];state.selText=' pasted';}
  if(mode==='paste-utf16-split'){state.value='a😀b';state.sel=[3,3];state.selText='\uDE00';}
  if(['paste-contains-unrelated','paste-long-selection','paste-noop','paste-utf16-split'].includes(mode)){
    // The observation must match the changed field so the value guard is not what refuses.
    const o=sandbox();vm.runInContext(observeScript,o);request.baseRoot=JSON.parse(o.run([JSON.stringify(request)])).root;
  }
  const actor=sandbox();vm.runInContext(actionScript,actor);
  const result=JSON.parse(actor.run([JSON.stringify(request)]));
  const refused=['reorder','ambiguous','pid-reused','foreground-changed','value-reused','focused-other-number','same-geometry-no-number','geometry-same-winid-different','incomplete-census','partial-census-duplicate','partial-census-unreadable-bounds','partial-census-ancestor-cut','disabled-target','press-unavailable-no-fallback','drag-target-value-changed','drag-target-disabled'];
  if(refused.includes(mode)){assert.equal(result.delivery,'not_dispatched',mode+': '+result.reason);assert.equal(effects.length,0,mode);assert.equal(state.writes,0,mode);}
  if(mode==='geometry-same-winid-different') assert.match(result.reason,/window correspondence/);
  // A twin inside the partial census is refused by the uniqueness rule, not by the census rule.
  if(mode==='partial-census-duplicate') assert.match(result.reason,/ambiguous/);
  if(mode==='partial-census-unreadable-bounds' || mode==='partial-census-ancestor-cut') assert.match(result.reason,/census is partial/);
  const beforeWrite=['paste-selection-unavailable','paste-selection-ambiguous','paste-noop','paste-utf16-split'];
  if(beforeWrite.includes(mode)){assert.equal(result.delivery,'not_dispatched',mode+': '+result.reason);assert.equal(state.pbWrites,0,mode);assert.equal(effects.length,0);assert.equal(state.clipboard,'original');}
  if(mode==='normal' || mode==='geometry-unique-no-number' || mode==='partial-census-elsewhere'){assert.equal(result.delivery,'submitted',result.reason);assert.equal(result.status,'unverified');assert.deepEqual(effects,['AXPress']);}
  if(mode==='press-throws-after-effect'){assert.equal(result.delivery,'unknown');assert.deepEqual(effects,['AXPress']);assert.equal(result.ok,false);}
  if(mode==='long-full-value'){assert.equal(result.status,'verified');assert.equal(state.value,request.value);assert.equal(state.writes,1);}
  if(mode==='setvalue-normalized'){assert.equal(result.status,'unverified');assert.equal(result.delivery,'submitted');assert.equal(state.writes,1);}
  if(mode==='write-throws-after-effect'){assert.equal(result.delivery,'unknown');assert.equal(state.writes,1);assert.equal(effects.length,0);}
  if(mode==='drag-normal'){assert.equal(result.delivery,'submitted',result.reason);assert.equal(effects.filter(e=>e==='CGEvent').length,7);}
  if(mode==='clipboard-conflict'){assert.equal(result.clipboard.restored,false);assert.equal(result.cleanup,'incomplete');assert.equal(state.clipboard,'user copied text');assert.deepEqual(effects,['key:v']);assert.equal(result.status,'unverified');}
  if(mode==='clipboard-write-fails'){assert.equal(result.delivery,'unknown');assert.equal(result.cleanup,'incomplete');assert.equal(result.clipboard.restored,false);assert.equal(effects.length,0);}
  if(mode==='clipboard-too-large'){assert.equal(result.delivery,'not_dispatched');assert.equal(effects.length,0);assert.equal(state.clipboard,'original');}
  if(['paste-unconfirmed','paste-contains-unrelated','paste-extra-suffix'].includes(mode)){assert.equal(result.clipboard.restored,false,mode);assert.equal(result.cleanup,'incomplete');assert.equal(state.clipboard,request.text);assert.equal(result.status,'unverified');assert.equal(result.delivery,'submitted');}
  if(mode==='paste-confirmed'){assert.equal(result.status,'verified',result.reason);assert.equal(state.value,'before pasted');assert.equal(result.clipboard.restored,true);assert.equal(state.clipboard,'original');}
  if(mode==='paste-long-selection'){assert.equal(result.status,'verified',result.reason);assert.equal(state.value,'가'.repeat(1000)+' pasted');assert.equal(state.clipboard,'original');}
  if(mode==='paste-selection-changed'){assert.equal(result.delivery,'not_dispatched',result.reason);assert.equal(effects.length,0);assert.equal(result.clipboard.restored,true);assert.equal(result.cleanup,'completed');assert.equal(state.clipboard,'original');}
  if(mode==='ancestor-reused'){assert.equal(result.delivery,'not_dispatched');assert.equal(result.issuedUnits,0);assert.deepEqual(effects,[]);assert.match(result.reason,/ancestor/);}
  if(mode==='disable-after-first-key'){assert.deepEqual(effects,['keyCode']);assert.equal(result.delivery,'unknown');assert.equal(result.issuedUnits,null);assert.equal(text.enabledFlag,false);}
  if(mode==='paste-throws-after-effect'){assert.deepEqual(effects,['key:v']);assert.equal(result.delivery,'unknown');assert.equal(result.issuedUnits,null);assert.equal(result.cleanup,'incomplete');assert.equal(result.clipboard.restored,false);assert.equal(state.clipboard,request.text);}
  if(mode.startsWith('secure-')) {
    if(mode==='secure-bound-inactive'){assert.equal(result.delivery,'submitted');assert.deepEqual(effects,['key:x']);}
    else {assert.equal(result.delivery,'not_dispatched');assert.equal(result.issuedUnits,0);assert.deepEqual(effects,[]);assert.match(result.reason,/Secure Event Input/);}
  }
  return {mode,result,effects,clipboard:state.clipboard,writes:state.writes};
}
const modes=['metadata-settable-true','metadata-settable-false','metadata-settable-unavailable','ancestor-reused','disable-after-first-key','paste-throws-after-effect','secure-unavailable','secure-binding-noop','secure-bound-active','secure-bound-inactive','normal','reorder','ambiguous','value-reused','pid-reused','foreground-changed','press-throws-after-effect','long-full-value','write-throws-after-effect','clipboard-conflict','clipboard-too-large','clipboard-write-fails','paste-unconfirmed','paste-confirmed',
  'focused-other-number','same-geometry-no-number','geometry-unique-no-number','geometry-same-winid-different','incomplete-census','partial-census-elsewhere','partial-census-duplicate','partial-census-unreadable-bounds','partial-census-ancestor-cut','disabled-target','press-unavailable-no-fallback','setvalue-normalized',
  'drag-normal','drag-target-value-changed','drag-target-disabled',
  'paste-contains-unrelated','paste-extra-suffix','paste-long-selection','paste-selection-unavailable','paste-selection-ambiguous','paste-noop','paste-utf16-split','paste-selection-changed',
  'restore-normal','restore-postcondition-fails','restore-focus-other'];
const results=[];let failed=0;
for(const mode of modes){try{results.push(runCase(mode));}catch(e){failed++;results.push({mode,failure:String(e&&e.stack||e)});}}
return JSON.stringify({total:results.length,failed,cases:results});
}

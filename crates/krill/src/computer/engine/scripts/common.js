// Portions adapted from Orca (MIT, Copyright (c) 2026 Lovecast Inc.); see engine.rs.
// Shared prefix of the observation and action JXA scripts: one copy of process,
// window and element identity so the two scripts cannot drift apart.
ObjC.import("AppKit");
ObjC.import("ApplicationServices");
ObjC.import("CoreGraphics");
ObjC.import("Foundation");
var req = null;
var se = Application("System Events");
function parseRequest(argv) {
  if(String(argv[0]) === "--request-file") {
    const data=$.NSData.dataWithContentsOfFile(String(argv[1] || ""));
    if(!data || data.isNil() || Number(data.length)>4194304) throw Error("private request file unavailable or exceeds bound");
    return JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data,$.NSUTF8StringEncoding)));
  }
  return JSON.parse(String(argv[0] || "{}"));
}
function processStart() {
  const running = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(req.pid));
  if (!running || running.isNil()) throw Error("target process exited");
  if (Number(running.processIdentifier) === Number($.NSProcessInfo.processInfo.processIdentifier)) throw Error("executor self target");
  const bundle = ObjC.unwrap(running.bundleIdentifier) || "";
  if (bundle === "dev.krill.app" || bundle === "com.krill.app") throw Error("Krill self target");
  const executable=running.executableURL ? ObjC.unwrap(running.executableURL.lastPathComponent) : "";
  if(executable === "krill-app" || executable === "Krill") throw Error("Krill executable self target");
  if (req.bundleId && bundle !== req.bundleId) throw Error("process bundle changed");
  if (!running.launchDate || running.launchDate.isNil()) throw Error("process start witness unavailable");
  return String(Number(running.launchDate.timeIntervalSince1970));
}
function getProcess() {
  const targetPid = Number(req.pid);
  if (!(targetPid > 0)) throw Error("missing process PID");
  const proc = se.processes.whose({ unixId: targetPid }).first;
  if (Number(proc.unixId()) !== targetPid) throw Error("process PID mismatch");
  return proc;
}
function rect(el) {
  const p = el.position(), z = el.size();
  return {x:Number(p[0]),y:Number(p[1]),width:Number(z[0]),height:Number(z[1])};
}
function canonical(value) {
  if(Array.isArray(value)) return value.map(canonical);
  if(value !== null && typeof value === "object") {
    const result={};Object.keys(value).sort().forEach(key=>{result[key]=canonical(value[key]);});return result;
  }
  return value;
}
function equal(a,b) { return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b)); }
function windowNumber(w) {
  try { const n=w.attributes.byName("AXWindowNumber").value(); return n === null || n === undefined ? null : String(n); } catch (_) { return null; }
}
function windowTitle(w) {
  try { return String(w.title() || ""); } catch (_) {}
  try { return String(w.name() || ""); } catch (_) {}
  return null;
}
function windowCensus(proc) {
  const wins = proc.windows;
  const names = wins.name();
  if (!Array.isArray(names) || names.length === 0) throw Error("AX windows unavailable; observation failed");
  if (names.length > 128) throw Error("AX window census exceeds bound; window identity unprovable");
  const rows=[];
  for (let i=0;i<names.length;i++) {
    const w=wins[i];
    let bounds=null;
    try { bounds=rect(w); } catch (_) {}
    rows.push({w,number:windowNumber(w),title:String(names[i] || ""),bounds});
  }
  return rows;
}
function titleAndGeometry(title,bounds) {
  return (!req.winTitle || title === req.winTitle) && bounds !== null && equal(bounds,req.winBounds);
}
// Title+geometry is a window identity only when the complete census has exactly one
// such window and no window-number witness contradicts it. Same bounds never suffice.
function uniqueGeometricWindow(rows) {
  const same=rows.filter(r=>titleAndGeometry(r.title,r.bounds));
  if (same.length !== 1 || (same[0].number !== null && same[0].number !== String(req.winId))) return null;
  return same[0];
}
function selectTargetWindow(proc) {
  const rows=windowCensus(proc);
  const numbered=rows.filter(r=>r.number === String(req.winId));
  if (numbered.length === 1) return numbered[0].w;
  const geometric=numbered.length ? null : uniqueGeometricWindow(rows);
  if (!geometric) throw Error("window correspondence missing or ambiguous");
  return geometric.w;
}
// Positive identity of two AX window references against the observed window.
function sameObservedWindow(proc, a, b) {
  const na=windowNumber(a), nb=windowNumber(b);
  if (na !== null || nb !== null) return na !== null && na === nb && na === String(req.winId);
  const geometric=uniqueGeometricWindow(windowCensus(proc));
  if (!geometric) return false;
  try {
    return titleAndGeometry(windowTitle(a),rect(a)) && titleAndGeometry(windowTitle(b),rect(b));
  } catch (_) { return false; }
}
function foreground(proc, win) {
  const front=$.NSWorkspace.sharedWorkspace.frontmostApplication;
  if(!front || Number(front.processIdentifier)!==Number(req.pid) || !Boolean(proc.frontmost())) throw Error("foreground application changed");
  let focused=null;
  try { focused=proc.attributes.byName("AXFocusedWindow").value(); } catch (_) {}
  if(!focused) throw Error("focused window unavailable");
  if(!sameObservedWindow(proc,focused,win)) throw Error("focused window is not positively the observed window");
}
// kCGWindowListOptionAll (0) finds a hidden/minimized window before restore;
// every input path requires kCGWindowListOptionOnScreenOnly (1).
function checkCGWindow(includeOffscreen) {
  const raw = $.CGWindowListCopyWindowInfo(includeOffscreen ? 0 : 1,0);
  const rows = raw ? ObjC.deepUnwrap(ObjC.castRefToObject(raw)) : [];
  const matches = rows.filter(w => String(w.kCGWindowNumber) === String(req.winId) && Number(w.kCGWindowOwnerPID) === Number(req.pid));
  if (matches.length !== 1) throw Error("window identity expired");
  const b=matches[0].kCGWindowBounds;
  if (!equal({x:Number(b.X),y:Number(b.Y),width:Number(b.Width),height:Number(b.Height)},req.winBounds)) throw Error("window geometry changed");
}
var rolesInteractive={AXButton:1,AXTextField:1,AXTextArea:1,AXCheckBox:1,AXRadioButton:1,AXPopUpButton:1,AXComboBox:1,AXMenuItem:1,AXLink:1,AXTab:1,AXSlider:1,AXRow:1,AXCell:1};
var truncated=false, budget=160, nextRef=1, entries={}, nodeHandles=new WeakMap(), targetPaths={};
function scalarValue(raw) {
  return (raw !== null && raw !== undefined && ["string","number","boolean"].indexOf(typeof raw)>=0) ? String(raw) : null;
}
function liveValue(el) {
  let raw=null;
  try { raw=el.value(); } catch (_) {}
  return scalarValue(raw);
}
function observedValueSettable(el) {
  try {
    const capability=el.attributes.byName("AXValue").settable();
    return typeof capability === "boolean" ? capability : null;
  } catch (_) { return null; }
}
function readNode(el, depth, isRoot) {
  function read(f, fallback) { try { const v=f(); return v === undefined ? fallback : v; } catch (_) { return fallback; } }
  const role=String(read(()=>el.role(),isRoot ? "AXWindow" : "AXGroup"));
  const actions=read(()=>el.actions.name(),[]).map(String);
  const value=liveValue(el);
  if (value !== null && value.length > 65536) throw Error("AX value exceeds bounded full observation; no lossy identity witness");
  const node={referenceId:null,role,subrole:read(()=>el.subrole(),null),title:read(()=>el.title(),null),description:read(()=>el.description(),null),value,valueSettable:observedValueSettable(el),
    bounds:read(()=>rect(el),null),enabled:Boolean(read(()=>el.enabled(),false)),focused:Boolean(read(()=>el.focused(),false)),selected:false,actions,children:[]};
  nodeHandles.set(node,el);
  if (!isRoot && (rolesInteractive[role] || actions.length)) {node.referenceId=nextRef++;entries[node.referenceId]=el;}
  const children=el.uiElements;
  let childRoles;
  try { childRoles=children.role(); } catch (_) {truncated=true;return node;}
  if (!Array.isArray(childRoles)) {truncated=true;return node;}
  if(isRoot && childRoles.length===0) truncated=true;
  if (depth >= 4 || budget <= 0) {if (childRoles.length) truncated=true;return node;}
  const count=Math.min(childRoles.length,40);
  if (count<childRoles.length) truncated=true;
  for(let i=0;i<count;i++) {
    if(budget--<=0) {truncated=true;break;}
    node.children.push(readNode(children[i],depth+1,false));
  }
  return node;
}
function identity(n, recursive) {
  const v=[n.role,n.subrole,n.title,n.description,n.bounds,n.actions];
  if(recursive) v.push(n.children.map(c=>identity(c,true)));
  return v;
}
function findRef(n, ref) {
  if (n.referenceId === ref) return n;
  for (let i=0;i<n.children.length;i++) { const found=findRef(n.children[i],ref); if (found) return found; }
  return null;
}
function pathToRef(n,ref) {
  if(n.referenceId===ref) return [n];
  for(let i=0;i<n.children.length;i++) {
    const path=pathToRef(n.children[i],ref);
    if(path) return [n].concat(path);
  }
  return null;
}
// Every reference the request names (targetRef and targetRefs), deduplicated.
function requestRefs() {
  const raw=(Array.isArray(req.targetRefs)?req.targetRefs:[]).concat(req.targetRef === null || req.targetRef === undefined ? [] : [req.targetRef]);
  const refs=[];
  raw.forEach(r=>{
    const ref=Number(r);
    if(!Number.isInteger(ref) || ref<1) throw Error("invalid element reference");
    if(refs.indexOf(ref)<0) refs.push(ref);
  });
  return refs;
}
// The fresh node must sit at the SAME reference/position as the original, be the only
// node with its identity, be enabled and hold the identical full value. Never rebind.
function validateTree(root) {
  if (!req.baseRoot || !equal(identity(root,true),identity(req.baseRoot,true))) throw Error("snapshot structure changed or reordered");
  const refs=requestRefs();
  if (refs.length && truncated) throw Error("bounded AX census is partial; target uniqueness unprovable");
  refs.forEach(ref=>{
    const target=findRef(req.baseRoot,ref);
    if(!target) throw Error("reference absent from immutable snapshot");
    const fresh=findRef(root,ref), matches=[];
    (function walk(n) {if(equal(identity(n,false),identity(target,false))) matches.push(n);n.children.forEach(walk);})(root);
    if(!fresh || matches.length!==1 || matches[0]!==fresh || !entries[ref]) throw Error("element identity missing or ambiguous");
    if(!fresh.enabled) throw Error("target element disabled");
    if(fresh.value!==target.value) throw Error("target value changed since observation; possible semantic reuse");
    const before=pathToRef(req.baseRoot,ref), after=pathToRef(root,ref);
    if(!before || !after || before.length!==after.length) throw Error("target ancestry changed");
    targetPaths[ref]=before.map((node,index)=>{
      if(node.value!==after[index].value || node.enabled!==after[index].enabled) throw Error("target ancestor value or enabled state changed");
      return {node,el:nodeHandles.get(after[index]),isTarget:index===before.length-1};
    });
  });
  return refs;
}
// Re-read live enabled/full value of every named target immediately before dispatch.
function checkTargetsLive(refs,allowOwnValueChange) {
  refs.forEach(ref=>{
    const target=findRef(req.baseRoot,ref), el=entries[ref];
    if(!target || !el) throw Error("element identity missing or ambiguous");
    let enabled=false;
    try { enabled=Boolean(el.enabled()); } catch (_) {}
    if(!enabled) throw Error("target element disabled");
    (targetPaths[ref] || []).forEach(witness=>{
      if(!witness.el) throw Error("target ancestry witness unavailable");
      let currentEnabled=false;
      try {currentEnabled=Boolean(witness.el.enabled());} catch(_) {}
      if(currentEnabled!==witness.node.enabled) throw Error("target ancestor enabled state changed");
      if(!(allowOwnValueChange && witness.isTarget) && liveValue(witness.el)!==witness.node.value) throw Error("target or ancestor value changed since observation; possible semantic reuse");
    });
  });
}

// Restore is an explicit mutation: the caller holds the global input lease and
// processStart() has already refused executor/Krill targets. Success requires the
// actual postcondition; after any mutation a failure is reported as unknown, never
// retried and never replaced by another strategy.
function restoreWindow(proc, win) {
  let touched=false;
  try {
    const minimized=win.attributes.byName("AXMinimized");
    if(!Boolean(proc.visible())) {touched=true;proc.visible=true;}
    if(Boolean(minimized.value())) {touched=true;minimized.value=false;}
    touched=true;
    proc.frontmost=true;
    if(win.actions.name().indexOf("AXRaise")>=0) win.actions.byName("AXRaise").perform();
    const deadline=Date.now()+1500;
    for(;;) {
      let failure=null;
      try {
        if(Boolean(win.attributes.byName("AXMinimized").value())) throw Error("window still minimized");
        checkCGWindow(false);
        foreground(proc,win);
      } catch (error) { failure=error; }
      if(!failure) return;
      if(Date.now()>=deadline) throw failure;
      $.NSThread.sleepForTimeInterval(0.05);
    }
  } catch (error) {
    throw Error((touched ? "restore outcome unknown; window/foreground mutation may have applied; no fallback: " : "restore not dispatched: ")+String(error));
  }
}
function run(argv) {
  req=parseRequest(argv);
  const start = processStart();
  checkCGWindow(Boolean(req.restore));
  const proc = getProcess();
  const win = selectTargetWindow(proc);
  if(req.restore) restoreWindow(proc, win);
  let geometryBefore=null, geometryAfter=null, geometryError=null;
  try { geometryBefore=captureGeometryFacts(req.pid,req.winId); }
  catch (_) { geometryError="display/window facts unavailable before AX observation"; }
  const root = readNode(win,0,true);
  try { geometryAfter=captureGeometryFacts(req.pid,req.winId); }
  catch (_) { geometryError="display/window facts unavailable after AX observation"; }
  if(root.role!=="AXWindow") throw Error("AX window not ready");
  return JSON.stringify({root,processStart:start,truncated,geometryBefore,geometryAfter,geometryError});
}

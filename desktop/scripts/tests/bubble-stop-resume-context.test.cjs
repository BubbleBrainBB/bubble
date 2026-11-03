const { app } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = process.env.BUBBLE_PACKAGE_APP_PATH || path.resolve(__dirname, '../..');
const home = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'bubble-stop-context-'));
app.setPath('userData', path.join(home, 'desktop'));
app.setAppPath(root);
process.env.BUBBLE_HOME = path.join(home, 'agent');
fs.mkdirSync(process.env.BUBBLE_HOME);
fs.writeFileSync(path.join(home, 'fact.txt'), 'TOOL_FACT_84721');
fs.writeFileSync(path.join(process.env.BUBBLE_HOME, 'config.json'), JSON.stringify({defaultProvider:'local-test',defaultModel:'local-test:test',providers:[{id:'local-test',enabled:true,apiKey:'fixture',baseURL:'http://127.0.0.1:1/v1',protocol:'openai'}]}));
fs.writeFileSync(path.join(process.env.BUBBLE_HOME, 'models.json'),JSON.stringify({providers:{'local-test':{baseURL:'http://127.0.0.1:1/v1',apiKey:'fixture',models:[{id:'test'}]}}}));
const delay = ms => new Promise(r => setTimeout(r,ms));
const until = async (f,label) => {for(let i=0;i<500;i++){if(f())return;await delay(10)}throw Error('Timeout: '+label)};
let adapter,sdk;
const events=[],requests=[],reports=[];
const timeout=setTimeout(()=>{console.error('Timed out');app.exit(1)},30000);
app.whenReady().then(async()=>{
 const {getBubbleSdk}=require(path.join(root,'dist-electron/electron/libs/provider/bubble-sdk-loader.js'));
 const {BubbleSdkAdapter}=require(path.join(root,'dist-electron/electron/libs/provider/bubble-sdk-adapter.js'));
 sdk=await getBubbleSdk(home);
 sdk.resolveProvider=()=>({providerId:'local-test',model:'local-test:test',provider:{
  async *streamChat(messages,options){
   const prompt=[...messages].reverse().find(m=>m.role==='user')?.content;
   requests.push({prompt,messages:JSON.parse(JSON.stringify(messages))});
   if(prompt.startsWith('cut:')){
    // Tool calls still being written when the user stops.
    yield {type:'tool_call',isStart:true,isEnd:false,id:'cut-widget',name:'show_widget',arguments:'{"title":"Cut","html":"<div>half'};
    yield {type:'tool_call',isStart:false,isEnd:false,id:'cut-widget',name:'show_widget',arguments:' more'};
    yield {type:'tool_call',isStart:true,isEnd:false,id:'cut-bash',name:'bash',arguments:'{"command":"ls -'};
    await new Promise((_,reject)=>{
     if(options.abortSignal.aborted)reject(options.abortSignal.reason);else options.abortSignal.addEventListener('abort',()=>reject(options.abortSignal.reason),{once:true});
    });
   }
   if(prompt.startsWith('work:')){
    if(!messages.some(m=>m.role==='tool'&&m.content.includes('TOOL_FACT_84721'))){
     yield {type:'tool_call',isStart:true,isEnd:true,id:'read-fact',name:'read',arguments:JSON.stringify({path:path.join(home,'fact.txt')})};
     yield {type:'done'};return;
    }
    yield {type:'text',content:'PARTIAL_PROGRESS_63579'};
    await new Promise((_,reject)=>{
     const fail=()=>setTimeout(()=>reject(options.abortSignal.reason),500);
     if(options.abortSignal.aborted)fail();else options.abortSignal.addEventListener('abort',fail,{once:true});
    });
   } else yield {type:'text',content:'reply:'+prompt};
   yield {type:'done'};
  },async complete(){return ''}
 }});
 adapter=new BubbleSdkAdapter();adapter.events.on('event',e=>events.push(e));
 const status=(id,s,n)=>events.slice(n).some(e=>e.threadId===id&&e.type==='status_change'&&e.status===s);
 for(const immediate of [false,true]){
  const id=immediate?'immediate':'settled';
  const session=await adapter.startSession({threadId:id,provider:'bubble',cwd:home,prompt:'remember:USER_FACT_92384',model:'local-test:test',bubblePermissionMode:'bypassPermissions'});
  await until(()=>status(id,'completed',0),'first turn');
  await adapter.sendTurn({threadId:id,prompt:'work:'+id});
  await until(()=>events.some(e=>e.threadId===id&&JSON.stringify(e).includes('PARTIAL_PROGRESS_63579')),'partial output');
  await adapter.stopSession(id);
  const stopReturnedPhase=sdk.getSessionRunState(session.providerSessionId).phase;
  if(!immediate)await until(()=>!sdk.getSessionRunState(session.providerSessionId).active,'SDK stop settles');
  const eventStart=events.length;
  const resumed=await adapter.startSession({threadId:id,provider:'bubble',cwd:home,resumeSessionId:session.providerSessionId,prompt:'continue:'+id,model:'local-test:test',bubblePermissionMode:'bypassPermissions'});
  await until(()=>status(id,'completed',eventStart)||status(id,'error',eventStart),'continued result');
  await until(()=>!sdk.getSessionRunState(session.providerSessionId).active,'final idle');
  const request=requests.find(r=>r.prompt==='continue:'+id);
  const text=JSON.stringify(request?.messages||[]);
  reports.push({scenario:id,stopReturnedPhase,sameSessionId:resumed.providerSessionId===session.providerSessionId,result:status(id,'completed',eventStart)?'completed':'error',modelReceivedContinue:!!request,userFact:text.includes('USER_FACT_92384'),toolFact:text.includes('TOOL_FACT_84721'),partialProgress:text.includes('PARTIAL_PROGRESS_63579'),errors:events.slice(eventStart).filter(e=>e.type==='error').map(e=>e.error.message),durableRoles:sdk.getHistory(session.providerSessionId).map(m=>m.role)});
 }
 // Stopped while the model is still writing tool calls: through the real stop,
 // each closes as interrupted under its id (a widget keeps only its title, no
 // half-written HTML), so what the chat shows is what history replays.
 {
  const id='cut';
  const toolInput=(toolId)=>events.filter(e=>e.threadId===id&&e.type==='message'&&e.message.message?.content?.[0]?.id===toolId).at(-1)?.message.message.content[0].input;
  await adapter.startSession({threadId:id,provider:'bubble',cwd:home,prompt:'cut:'+id,model:'local-test:test',bubblePermissionMode:'bypassPermissions'});
  await until(()=>toolInput('cut-bash')?.__aegisToolCallStreaming===true&&toolInput('cut-widget')?.html,'partial tool calls drawn');
  await adapter.stopSession(id);
  assert.deepEqual(toolInput('cut-widget'),{title:'Cut',__aegisToolCallInterrupted:true},'the cut widget closes with its title only');
  assert.equal(toolInput('cut-bash')?.__aegisToolCallInterrupted,true,'the cut command closes as interrupted');
  assert.equal(toolInput('cut-bash')?.__aegisToolCallStreaming,undefined);
 }
 const {runAgentLoop,ensureProviderService}=require(path.join(root,'dist-electron/electron/libs/agent-loop.js'));
 const {getProviderService}=require(path.join(root,'dist-electron/electron/libs/provider/service.js'));
 ensureProviderService();const service=getProviderService();service.events.on('event',e=>events.push(e));
 const runnerId='runner-immediate';let nativeId;const runnerErrors=[];
 const opts={session:{id:runnerId,provider:'bubble',cwd:home},model:'local-test:test',bubblePermissionMode:'bypassPermissions',onMessage:m=>{if(m.type==='system'&&m.subtype==='init')nativeId=m.session_id},onError:e=>runnerErrors.push(e.message),onPermissionRequest:async()=>({behavior:'allow'})};
 const handle=runAgentLoop({...opts,prompt:'remember:USER_FACT_92384'});
 await until(()=>status(runnerId,'completed',0),'runner first turn');
 handle.send('work:'+runnerId);
 await until(()=>events.some(e=>e.threadId===runnerId&&JSON.stringify(e).includes('PARTIAL_PROGRESS_63579')),'runner partial');
 handle.abort();
 const afterAbort=events.length;
 const replacement=runAgentLoop({...opts,resumeSessionId:nativeId,prompt:'continue:'+runnerId});
 await until(()=>status(runnerId,'completed',afterAbort)||runnerErrors.length,'runner continued result');
 await until(()=>!sdk.getSessionRunState(nativeId).active,'runner idle');
 const rr=requests.find(r=>r.prompt==='continue:'+runnerId);
 reports.push({scenario:'desktop-runner-immediate',modelReceivedContinue:!!rr,errors:runnerErrors,historyStillContainsOriginalFact:JSON.stringify(sdk.getHistory(nativeId)).includes('USER_FACT_92384'),historyStillContainsToolFact:JSON.stringify(sdk.getHistory(nativeId)).includes('TOOL_FACT_84721')});
 replacement.abort();
 // Hold the real SDK at its asynchronous initialization boundary before the first prompt is persisted.
 const originalTrust=sdk.resolveProjectTrust.bind(sdk);
 let setupEntered=false;
 sdk.resolveProjectTrust=async(cwd,options,signal)=>{
  if(options.prompt==='initial:BOOT_FACT_51932'){
   setupEntered=true;
   await new Promise((_,reject)=>{const fail=()=>reject(signal.reason);if(signal.aborted)fail();else signal.addEventListener('abort',fail,{once:true})});
  }
  return originalTrust(cwd,options,signal);
 };
 const early=await adapter.startSession({threadId:'early',provider:'bubble',cwd:home,prompt:'initial:BOOT_FACT_51932',model:'local-test:test'});
 await until(()=>setupEntered,'early setup');
 const wasOnDisk=sdk.listSessions().some(s=>s.name===early.providerSessionId);
 await adapter.stopSession('early');
 await until(()=>!sdk.getSessionRunState(early.providerSessionId).active,'early stop');
 const earlyEvents=events.length;
 const earlyResume=await adapter.startSession({threadId:'early',provider:'bubble',cwd:home,resumeSessionId:early.providerSessionId,prompt:'continue:early',model:'local-test:test'});
 await until(()=>status('early','completed',earlyEvents)||status('early','error',earlyEvents),'early resume');
 const earlyRequest=requests.find(r=>r.prompt==='continue:early');
 reports.push({scenario:'stopped-during-first-initialization',originalSessionPersisted:wasOnDisk,sameSessionId:earlyResume.providerSessionId===early.providerSessionId,modelReceivedContinue:!!earlyRequest,modelReceivedOriginalTask:JSON.stringify(earlyRequest?.messages||[]).includes('BOOT_FACT_51932')});
 sdk.resolveProjectTrust=originalTrust;
 for (const report of reports.slice(0,2)) {
  assert.equal(report.stopReturnedPhase, 'idle', 'stop resolves only after SDK cleanup');
  assert.equal(report.sameSessionId, true);
  assert.equal(report.result, 'completed');
  assert.equal(report.modelReceivedContinue, true);
  assert.equal(report.userFact, true);
  assert.equal(report.toolFact, true);
  assert.equal(report.partialProgress, true);
  assert.deepEqual(report.errors, []);
 }
 const runnerReport=reports.find(r=>r.scenario==='desktop-runner-immediate');
 assert.equal(runnerReport.modelReceivedContinue,true);
 assert.deepEqual(runnerReport.errors,[]);
 assert.equal(runnerReport.historyStillContainsOriginalFact,true);
 assert.equal(runnerReport.historyStillContainsToolFact,true);
 const earlyReport=reports.find(r=>r.scenario==='stopped-during-first-initialization');
 assert.equal(earlyReport.sameSessionId,true);
 assert.equal(earlyReport.modelReceivedOriginalTask,true);
 await assert.rejects(adapter.startSession({threadId:'missing',provider:'bubble',cwd:home,resumeSessionId:'missing-history',prompt:'continue',model:'local-test:test'}),/saved history is missing/);
 console.log('PASS: settled and immediate stop/resume preserve original input, tool results and partial output; tool calls cut by a stop close as interrupted; runner stop gate; setup interruption; missing history fails explicitly.');
}).then(async()=>{await adapter?.stopAll();clearTimeout(timeout);fs.rmSync(home,{recursive:true,force:true});app.exit(0)},async error=>{console.error(error);await adapter?.stopAll();clearTimeout(timeout);fs.rmSync(home,{recursive:true,force:true});app.exit(1)});

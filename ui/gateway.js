/* Gateway views share the existing desktop theme, account store, language and time zone. */
(() => {
  let snapshot=null, pending=false, initialized=false, client='claude', reveal=false, selected=null, polling=false, formDirty=false;
  const $=id=>document.getElementById(id);
  const e=value=>esc(String(value??''));
  const text=(zh,en)=>tr(zh,en);
  const num=n=>Number(n||0).toLocaleString();
  const region=ed=>ed==='cn'?text('国内版','China'):text('国际版','International');
  const duration=n=>n==null?'—':n<1000?`${n} ms`:`${(n/1000).toFixed(2)} s`;
  const publicId=m=>m.publicId||m.name||m.id;
  const label=p=>({'anthropic-messages':'Messages','openai-responses':'Responses','chat-completions':'Chat Completions'}[p]||p);
  const status=s=>s==='completed'?text('成功','Success'):s==='interrupted'?text('已中断','Interrupted'):text('失败','Failed');
  const time=s=>{try{return fmtTimeStr(s);}catch{return new Date(s).toLocaleString();}};
  function navigate(page){document.querySelector(`.nav-item[data-page="${page}"]`)?.click();}
  async function copy(value){
    try {await navigator.clipboard.writeText(value);} catch {
      const area=document.createElement('textarea');area.value=value;area.style.position='fixed';area.style.opacity='0';document.body.append(area);area.select();const ok=document.execCommand('copy');area.remove();if(!ok)throw new Error(text('复制失败，请手动选择文本','Copy failed; select the text manually'));
    }
    toast(text('已复制','Copied'),true);
  }
  function empty(title,description,action,button){return `<div class="gateway-empty"><strong>${e(title)}</strong>${e(description)}${action?`<br><button class="primary" data-gateway-action="${action}">${e(button)}</button>`:''}</div>`;}
  function table(logs){
    if(!logs.length)return empty(text('还没有请求记录','No requests yet'),text('启动服务并连接客户端后，每次调用都会出现在这里。','Start the service and connect a client to see requests here.'));
    return `<div class="gateway-table-scroll"><table class="gateway-table"><thead><tr><th>${text('时间 / 模型','Time / model')}</th><th>${text('协议','Protocol')}</th><th>${text('账号','Account')}</th><th>${text('结果','Result')}</th><th>${text('用量','Tokens')}</th><th>${text('耗时','Duration')}</th></tr></thead><tbody>${logs.map(log=>`<tr data-request-id="${e(log.id)}" tabindex="0" role="button"><td>${e(log.model)}<small>${e(time(log.time))}</small></td><td>${e(label(log.protocol))}</td><td>${e(log.account||text('未分配账号','Unassigned'))}<small>${e(region(log.edition))}</small></td><td><span class="pill ${log.status==='completed'?'on':'off'}">${e(status(log.status))}</span></td><td class="num">${log.usage&&typeof log.usage.total_tokens==='number'?num(log.usage.total_tokens):'—'}</td><td class="num">${duration(log.durationMs)}</td></tr>`).join('')}</tbody></table></div>`;
  }
  function renderOverview(){
    const g=snapshot,c=g.config,logs=g.logs||[],success=logs.filter(l=>l.status==='completed');
    const accounts=(STATE?.accounts||[]).filter(a=>a.edition===c.edition&&a.hasToken&&!c.disabledAccounts.includes(a.id)&&(!a.expiresAt||new Date(a.expiresAt)>new Date()));
    $('gatewayHeroEndpoint').textContent=`http://127.0.0.1:${c.port}/v1`;
    $('gatewayHeroStatus').textContent=g.running?text('本地网关正在运行','LOCAL GATEWAY RUNNING'):text('本地网关待启动','LOCAL GATEWAY STOPPED');
    const tokens=logs.reduce((s,l)=>s+(Number(l.usage?.total_tokens)||0),0);
    const metrics=[[text('可用账号','Available accounts'),accounts.length,`${region(c.edition)} · ${text('账号池','Account pool')}`],[text('目录模型','Catalog models'),g.models.length,text('来自官方模型目录','Official model catalog')],[text('活动请求','Active requests'),g.active,text('每个账号最多 2 并发','Up to 2 requests per account')],[text('已记录 Tokens','Recorded tokens'),num(tokens),`${success.length} ${text('次成功请求 · 最近 200 条','successful requests · latest 200')}`]];
    $('gatewayMetrics').innerHTML=metrics.map(([name,value,note])=>`<div class="gateway-metric"><span>${e(name)}</span><strong>${e(value)}</strong><small>${e(note)}</small></div>`).join('');
    const rows=[[text('服务状态','Service'),g.running?text('正在监听','Listening'):text('未启动','Stopped')],[text('账号地区','Region'),region(c.edition)],[text('调度策略','Scheduling'),c.strategy==='roundRobin'?text('轮询','Round robin'):text('临期优先','Expiry first')],[text('最近请求成功率','Recorded success rate'),logs.length?`${Math.round(success.length/logs.length*100)}%`:'—']];
    $('gatewayRuntime').innerHTML=rows.map(([k,v])=>`<div class="gateway-info-row"><span>${e(k)}</span><b>${e(v)}</b></div>`).join('');
    $('gatewayRecent').innerHTML=table(logs.slice(0,5));
  }
  function renderModels(){
    const query=$('gatewayModelSearch').value.toLowerCase(),g=snapshot;
    const models=g.models.filter(m=>`${m.id} ${m.name}`.toLowerCase().includes(query));
    $('gatewayModelRegion').textContent=region(g.config.edition);
    $('gatewayModelUpdated').textContent=g.modelUpdated?`${text('更新于','Updated')} ${time(g.modelUpdated)}`:text('尚未同步','Not synced yet');
    if(!models.length){$('gatewayModels').innerHTML=empty(g.models.length?text('没有匹配模型','No matching models'):text('先同步你的模型目录','Sync your model catalog'),text('从所选地区账号获取模型，不会自动发起聊天请求。','Loads models for the selected account region without generating a response.'),'sync-models',text('同步模型目录','Sync catalog'));return;}
    $('gatewayModels').innerHTML=models.map(m=>{
      const verified=g.logs.some(l=>(l.model===m.id||l.model===publicId(m))&&l.edition===g.config.edition&&l.status==='completed');
      const tags=[...(m.reasoning?.supportedEfforts||[]).map(x=>x==='none'?text('可关闭思考','Thinking off'):`${text('思考','Effort')} ${x}`),m.supportsTools===true?text('工具调用','Tools'):null,m.supportsImages===true?text('图片','Images'):null,m.contextWindow?`${num(m.contextWindow)} context`:null].filter(Boolean);
      return `<article class="gateway-model"><div class="gateway-model-top"><span class="gateway-model-icon">◇</span><span class="pill ${verified?'on':''}">${verified?text('已调用','Called successfully'):text('目录可见','In catalog')}</span></div><h3>${e(m.name)}</h3><code>${e(publicId(m))}</code><div class="gateway-model-tags">${(tags.length?tags:[text('能力以实际调用为准','Capabilities depend on model')]).map(t=>`<span class="pill">${e(t)}</span>`).join('')}</div><div class="gateway-model-footer"><span class="gateway-subtle">${e(region(g.config.edition))} · Qoder</span><button data-model-copy="${e(publicId(m))}">${text('复制 ID','Copy ID')}</button></div></article>`;
    }).join('');
  }
  function renderRequests(){
    const q=$('gatewayRequestSearch').value.toLowerCase(),filter=$('gatewayRequestFilter').value;
    const logs=snapshot.logs.filter(l=>(filter==='all'||(filter==='failed'?l.status!=='completed':l.status===filter))&&`${l.model} ${l.account} ${l.protocol}`.toLowerCase().includes(q));
    $('gatewayRequests').innerHTML=table(logs);
  }
  function configCode(){
    const port=snapshot.config.port,model=(snapshot.models[0]?publicId(snapshot.models[0]):'<Qoder model ID>');
    if(client==='claude')return `# ${text('在终端配置你的本地密钥','Set your local key in the terminal')}\nexport ANTHROPIC_BASE_URL="http://127.0.0.1:${port}"\nexport ANTHROPIC_AUTH_TOKEN="<LOCAL_API_KEY>"\nexport ANTHROPIC_MODEL="${model}"\n\nclaude`;
    if(client==='codex')return `# ~/.codex/config.toml\nmodel = "${model}"\nmodel_provider = "qoder_gateway"\n\n[model_providers.qoder_gateway]\nname = "Qoder Gateway"\nbase_url = "http://127.0.0.1:${port}/v1"\nwire_api = "responses"\nenv_key = "QODER_GATEWAY_API_KEY"\n\n# ${text('终端中设置，勿粘贴到 TOML 文件','Set in your terminal, not in the TOML file')}:\n# export QODER_GATEWAY_API_KEY="<LOCAL_API_KEY>"`;
    return `Base URL: http://127.0.0.1:${port}/v1\nAPI Key: <LOCAL_API_KEY>\nModel: ${model}\n\ncurl http://127.0.0.1:${port}/v1/chat/completions \\\n  -H "Authorization: Bearer $QODER_GATEWAY_API_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d '${JSON.stringify({model,messages:[{role:'user',content:'Hello'}],stream:true})}'`;
  }
  function renderAccess(){
    const g=snapshot,c=g.config;
    if(!initialized&&!formDirty){$('gatewayPort').value=c.port;$('gatewayEdition').value=c.edition;$('gatewayStrategy').value=c.strategy;$('gatewayTransport').value=c.transport||'legacy';$('gatewayCnEndpoint').value=c.cnEndpoint;$('gatewayIntlEndpoint').value=c.intlEndpoint;initialized=true;}
    $('gatewayKey').textContent=reveal?g.apiKey:'••••••••••••••••••••••••';
    $('gatewayKeyInput').type=reveal?'text':'password';
    const revealButton=document.querySelector('[data-gateway-action="reveal-key"]');revealButton.textContent=reveal?text('隐藏','Hide'):text('显示','Reveal');
    $('gatewayAccessNotice').innerHTML=g.error?`<div class="gateway-notice">${e(g.error)}</div>`:'';
    $('gatewayClientCode').textContent=configCode();
    $('gatewayClientHint').textContent=client==='codex'?text('Responses 无状态模式：回传完整历史。支持 function、namespace 与 custom 工具转换；服务端工具和跨提供商加密推理历史不支持。','Stateless Responses with full history. Function, namespace, and custom tool mapping; no hosted tools or cross-provider encrypted reasoning.'):client==='claude'?text('使用本地 Key 替换 <LOCAL_API_KEY>，使用目录中的模型 ID。count_tokens 为本地估算。','Replace <LOCAL_API_KEY> with your local key and use a catalog model ID. Token counting is estimated.'):text('将本地网关地址和密钥填入兼容 OpenAI 的客户端。','Use the local base URL and key in an OpenAI-compatible client.');
    $('gatewayClientTabs').querySelectorAll('button').forEach(b=>b.classList.toggle('active',b.dataset.gatewayAction===client));
  }
  function renderAccountControls(){
    if(!snapshot)return;
    document.querySelectorAll('.gateway-slot').forEach(slot=>{
      const id=slot.dataset.account,entry=snapshot.accounts.find(a=>a.id===id);
      slot.replaceChildren();if(!entry)return;
      const row=document.createElement('div');row.className='gateway-account-control';
      const name=document.createElement('span');name.textContent=text('反代','Proxy');
      const button=document.createElement('button');button.type='button';button.className='pool-switch';button.setAttribute('role','switch');button.setAttribute('aria-checked',String(entry.enabled));button.setAttribute('aria-label',text('启用此账号反代','Enable proxy for account'));button.dataset.gatewayAccount=id;button.disabled=pending;
      const state=document.createElement('small');state.className='pool-concurrency';state.textContent=entry.cooldownSeconds?`${text('冷却','Cooldown')} ${entry.cooldownSeconds}s`:`${entry.active} / 2 ${text('并发','active')}`;
      row.append(name,button,state);slot.append(row);
    });
  }
  function render(){
    if(!snapshot)return;
    document.body.classList.toggle('gateway-running',snapshot.running);
    document.querySelectorAll('.gateway-start').forEach(button=>{button.textContent=snapshot.running?text('停止服务','Stop service'):text('启动服务','Start service');button.disabled=pending;});
    $('gatewaySidebarStatus').innerHTML=`<i></i> ${e(snapshot.running?text('本地服务运行中','Local service running'):text('服务未启动','Service stopped'))}`;
    renderOverview();renderModels();renderRequests();renderAccess();renderAccountControls();
  }
  async function refresh(){
    if(polling)return;polling=true;
    try{snapshot=await invoke('gateway_state');render();}catch(error){$('gatewayAccessNotice').innerHTML=`<div class="gateway-notice">${e(String(error))}</div>`;}finally{polling=false;}
  }
  async function action(task,button){
    if(pending)return;pending=true;if(button)button.disabled=true;
    try{await task();await refresh();}catch(error){toast(String(error),false);}finally{pending=false;if(button)button.disabled=false;render();}
  }
  function openRequest(id){
    selected=snapshot.logs.find(l=>l.id===id);if(!selected)return;
    $('gatewayDrawerTitle').textContent=text('请求详情','Request details');
    const fields=[[text('模型','Model'),selected.model],[text('协议','Protocol'),label(selected.protocol)],[text('账号','Account'),selected.account],[text('地区','Region'),region(selected.edition)],[text('时间','Time'),time(selected.time)],[text('结果','Result'),status(selected.status)],['HTTP',selected.httpStatus],[text('首字节','First byte'),duration(selected.firstByteMs)],[text('总耗时','Duration'),duration(selected.durationMs)],['Request ID',selected.requestId],[text('上游业务码','Upstream code'),selected.upstreamCode||'—']];
    $('gatewayDrawerBody').innerHTML=fields.map(([k,v])=>`<div class="gateway-info-row"><span>${e(k)}</span><b>${e(v)}</b></div>`).join('')+(selected.message?`<div class="gateway-notice" style="margin-top:18px">${e(selected.message)}</div>`:'')+`<pre class="gateway-code">${e(JSON.stringify(selected.usage||{},null,2))}</pre><p class="gateway-subtle">${text('仅记录数值与运行元数据，不保存对话正文或工具参数。','Only numeric usage and runtime metadata are recorded.')}</p>`;
    $('gatewayDrawer').hidden=false;$('gatewayDrawer').querySelector('button').focus();
  }
  document.addEventListener('click',async event=>{
    const request=event.target.closest('[data-request-id]');if(request){openRequest(request.dataset.requestId);return;}
    const model=event.target.closest('[data-model-copy]');if(model){await copy(model.dataset.modelCopy);return;}
    const account=event.target.closest('[data-gateway-account]');
    if(account&&snapshot){await action(async()=>{const config=structuredClone(snapshot.config),id=account.dataset.gatewayAccount;config.disabledAccounts=config.disabledAccounts.includes(id)?config.disabledAccounts.filter(x=>x!==id):[...config.disabledAccounts,id];await invoke('gateway_save',{config});},account);return;}
    const button=event.target.closest('[data-gateway-action]');if(!button)return;
    const task=button.dataset.gatewayAction;
    if(task==='close-drawer'){$('gatewayDrawer').hidden=true;return;}
    if(!snapshot)return;
    if(['claude','codex','openai'].includes(task)){client=task;navigate('access');renderAccess();return;}
    if(task==='requests'){navigate('requests');return;}
    if(task==='edit-key'){$('gatewayKeyEditor').hidden=false;$('gatewayKeyInput').value=snapshot.apiKey;$('gatewayKeyInput').focus();return;}
    if(task==='cancel-key'){$('gatewayKeyEditor').hidden=true;$('gatewayKeyInput').value='';return;}
    if(task==='random-key')return await action(async()=>{$('gatewayKeyInput').value=await invoke('gateway_generate_key');$('gatewayKeyEditor').hidden=false;$('gatewayKeyInput').focus();},button);
    if(task==='save-key')return await action(async()=>{await invoke('gateway_set_key',{apiKey:$('gatewayKeyInput').value});$('gatewayKeyInput').value='';$('gatewayKeyEditor').hidden=true;toast(text('密钥已保存，请更新客户端 API Key','Key saved. Update your clients.'),true);},button);
    if(task==='reveal-key'){reveal=!reveal;renderAccess();return;}
    try{
      if(task==='copy-base')return await copy(`http://127.0.0.1:${snapshot.config.port}/v1`);
      if(task==='copy-key')return await copy(snapshot.apiKey);
      if(task==='copy-config')return await copy(configCode());
      if(task==='copy-diagnostic')return await copy(JSON.stringify(selected,null,2));
      if(task==='toggle')return await action(async()=>{await invoke(snapshot.running?'gateway_stop':'gateway_start');toast(snapshot.running?text('正在停止，活动请求完成后关闭','Stopping after active requests finish'):text('本地服务已启动','Local service started'),true);},button);
      if(task==='sync-models')return await action(async()=>{button.textContent=text('正在同步…','Syncing…');const r=await invoke('gateway_models');toast(`${text('已同步','Synced')} ${r.count} ${text('个模型','models')}`,true);},button).finally(()=>{button.textContent=text('同步模型目录','Sync catalog');});
      if(task==='clear-logs')return await action(()=>invoke('gateway_clear_logs'),button);
    }catch(error){toast(String(error),false);}
  });
  document.addEventListener('keydown',event=>{const row=event.target.closest('[data-request-id]');if(row&&event.key==='Enter')openRequest(row.dataset.requestId);if(event.key==='Escape')$('gatewayDrawer').hidden=true;});
  $('gatewayDrawer').addEventListener('click',event=>{if(event.target===$('gatewayDrawer'))$('gatewayDrawer').hidden=true;});
  $('gatewayKeyInput').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();document.querySelector('[data-gateway-action="save-key"]').click();}if(event.key==='Escape'){event.preventDefault();document.querySelector('[data-gateway-action="cancel-key"]').click();}});
  $('gatewayConfigForm').addEventListener('input',()=>{formDirty=true;});
  $('gatewayConfigForm').addEventListener('submit',event=>{
    event.preventDefault();if(!snapshot)return;
    action(async()=>{const config={...snapshot.config,port:Number($('gatewayPort').value),edition:$('gatewayEdition').value,strategy:$('gatewayStrategy').value,transport:$('gatewayTransport').value,cnEndpoint:$('gatewayCnEndpoint').value.trim().replace(/\/$/,''),intlEndpoint:$('gatewayIntlEndpoint').value.trim().replace(/\/$/,'')};await invoke('gateway_save',{config});formDirty=false;initialized=false;toast(text('配置已保存','Configuration saved'),true);},event.submitter);
  });
  $('gatewayModelSearch').addEventListener('input',()=>snapshot&&renderModels());
  $('gatewayRequestSearch').addEventListener('input',()=>snapshot&&renderRequests());
  $('gatewayRequestFilter').addEventListener('change',()=>snapshot&&renderRequests());
  window.addEventListener('qoder:accounts',()=>{if(snapshot){renderAccountControls();renderOverview();}});
  refresh();setInterval(()=>{if(!document.hidden)refresh();},3000);
})();

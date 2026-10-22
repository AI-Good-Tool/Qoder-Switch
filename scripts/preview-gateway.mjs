// Development-only preview. Synthetic accounts and metadata; no official requests.
import http from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';
const root=resolve(import.meta.dirname,'../ui');
const fixture=`<script>
const config={transport:'legacy',port:24818,edition:'both',modelRoutes:{},strategy:'roundRobin',disabledAccounts:['cn:preview4'],cnEndpoint:'https://gateway.qoder.com.cn',intlEndpoint:'https://api2.qoder.sh'};
const accounts=['大飞龙','小米不是米','码上农业','李小米','李小茶','码农3号','青青李子','安好'].map((name,i)=>({id:'cn:preview'+i,uid:'07efe4fd-a82c-4270-af2f-'+i+'b16abcd',edition:i===7?'intl':'cn',name,email:'demo'+i+'@example.test',note:i===2?'研发工作区':'',phone:'',hasToken:true,expiresAt:'2027-01-01T00:00:00Z',isCurrent:i===1}));
const gateway={config,apiKey:'OFFLINE_PREVIEW_KEY_ONLY',running:false,active:0,accounts:accounts.map((a,i)=>({id:a.id,enabled:i!==4,active:i===1?1:0,cooldownSeconds:0})),models:[],logs:[],error:'',modelUpdated:null};
const catalogs={cn:[{id:'cn-shared',name:'Shared Model',supportsTools:true,contextWindow:200000},{id:'cn-only',name:'China Model'}],intl:[{id:'intl-shared',name:'Shared Model',supportsTools:true,contextWindow:200000},{id:'intl-only',name:'International Model'}]};
gateway.catalogs=catalogs;
const rows=[['Shared Model',[['cn','cn-shared'],['intl','intl-shared']]],['China Model',[['cn','cn-only']]],['International Model',[['intl','intl-only']]]];
function updateModels(){gateway.models=rows.map(([name,pairs])=>{const setting=config.modelRoutes[name];const targets=(setting?[['cn',setting.cn],['intl',setting.intl]]:pairs).filter(([ed,id])=>id).map(([ed,id])=>({...catalogs[ed].find(m=>m.id===id),edition:ed}));const active=targets.filter(t=>config.edition==='both'||config.edition===t.edition);return {routeId:name,id:targets[0]?.id,name,publicId:setting?.alias||name,allTargets:targets,targets:active,editions:active.map(t=>t.edition),available:active.length>0,supportsTools:true,contextWindow:200000};});}
updateModels();
gateway.logs=[{id:'demo-request',time:new Date().toISOString(),protocol:'openai-responses',model:'Shared Model',requestedModel:'Shared Model',realModel:'intl-shared',modelName:'Shared Model',edition:'intl',account:'Demo International',status:'completed',httpStatus:200,durationMs:1240,firstByteMs:230,usage:{prompt_tokens:100,completion_tokens:20,total_tokens:120},requestId:'offline-demo',attempt:2,clientRequestId:'offline-group'}];
gateway.logs.push({...gateway.logs[0],id:'demo-retry',requestId:'offline-retry',account:'Demo China',edition:'cn',realModel:'cn-shared',status:'retried',httpStatus:403,upstreamCode:'403',attempt:1,message:'Qoder 流式业务错误（403）；已尝试下一个符合路由的账号',usage:null});
window.__TAURI__={core:{invoke:async function(cmd,args){
if(cmd==='app_update_metadata')return {version:'1.8.4',target:'macos-aarch64',autoCheckUpdates:false};
if(cmd==='app_update_take_install_error')return null;
if(cmd==='app_update_check')return {currentVersion:'1.8.4',latestVersion:'1.8.5',hasUpdate:true,canDownload:true,target:'macos-aarch64',releaseNotes:'演示更新内容\\n改善账号管理与连接体验。'};
if(cmd==='app_update_download')return {version:'1.8.5',assetName:'Qoder.Switch_1.8.5_aarch64.dmg',path:'/offline-preview/installer.dmg'};
if(cmd==='app_update_cancel_download')return {};
if(cmd==='get_state')return {accounts,editions:{cn:{label:'Qoder 国内版',installed:true,running:false},intl:{label:'Qoder 国际版',installed:true,running:false}},settings:{theme:localStorage.getItem('qs-theme')||'light',lang:'zh',timezone:'system',tokenFmt:'km'},scheduler:{lastRun:null},claimLog:[]};
if(cmd==='token_stats'){const date=new Date().toISOString().slice(0,10);return {available:true, daily:[{date,tokens:12500,in:10000,out:2500,requests:2}],projects:[{name:'演示账号',tokens:12500,requests:2,daily:{[date]:{tokens:12500,requests:2}}}],models:[],requests:[{ts:date+' 12:00:00',model:'GLM-5.3-Flash',cwd:'演示账号',title:'messages · completed',inTokens:10000,outTokens:2500,durationMs:1900},{ts:date+' 12:01:00',model:'GLM-5.3-Flash',cwd:'演示账号',title:'responses · failed',inTokens:null,outTokens:null,durationMs:800}]};}
if(cmd==='credits')return {ok:true,data:{usage:{userQuota:{total:2000,remaining:1441.51,used:558.49},addOnQuota:{total:400,remaining:312.5,used:87.5}}}};
if(cmd==='gateway_generate_key')return 'qs_'+crypto.randomUUID().replaceAll('-','')+crypto.randomUUID().replaceAll('-','');
if(cmd==='gateway_set_key'){if(!/^[!-~]{32,256}$/.test(args.apiKey))throw new Error('API Key 需为 32–256 位 ASCII 可见字符，不能包含空格或换行');gateway.apiKey=args.apiKey;return {ok:true};}
if(cmd==='gateway_save_route'){if(!args.alias.trim())throw Error('请输入别名');if(!args.cn&&!args.intl)throw Error('至少选择一个地区');config.modelRoutes[args.routeId]={alias:args.alias,cn:args.cn,intl:args.intl};updateModels();return {ok:true};}
if(cmd==='gateway_state'){updateModels();gateway.accounts.forEach(a=>a.enabled=!config.disabledAccounts.includes(a.id));return structuredClone(gateway);}
if(cmd==='gateway_start'){gateway.running=true;return gateway;}
if(cmd==='gateway_stop'){gateway.running=false;return {};}
if(cmd==='gateway_save'){Object.assign(config,args.config);return gateway;}
if(cmd==='gateway_models'){updateModels();return {count:4,warnings:[]};}
if(cmd==='set_theme'){document.body.classList.toggle('dark',args.theme==='dark');return {};}
if(cmd==='set_setting')return {};
throw new Error('Offline preview: '+cmd+' unavailable');
}}};
</script>`;
http.createServer(async(req,res)=>{
try{const pathname=new URL(req.url,'http://localhost').pathname;const file=resolve(root,pathname==='/'?'index.html':'.'+pathname);if(!file.startsWith(root+'/'))throw Error();let bytes=await readFile(file);const ext=extname(file);if(ext==='.html')bytes=Buffer.from(bytes.toString().replace('<script src="gateway-shell.js">',fixture+'<script src="gateway-shell.js">'));res.setHeader('Content-Type',({'.html':'text/html','.js':'text/javascript','.css':'text/css'})[ext]||'application/octet-stream');res.end(bytes);}catch{res.writeHead(404);res.end();}
}).listen(24819,'127.0.0.1',()=>console.log('Offline UI preview http://127.0.0.1:24819'));

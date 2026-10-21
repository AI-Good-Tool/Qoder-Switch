#!/usr/bin/env python3
"""Bounded gateway smoke check. --live permits at most four 128-token model calls."""
import argparse, json, urllib.request, urllib.error
from pathlib import Path
from datetime import datetime, timezone

parser=argparse.ArgumentParser();parser.add_argument('--live',action='store_true');parser.add_argument('--port',type=int,default=24818);args=parser.parse_args()
base=f'http://127.0.0.1:{args.port}'
key=(Path.home()/'.qoder-switch/gateway/api-key').read_text().strip()
headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'}
report={'time':datetime.now(timezone.utc).isoformat(),'kind':'live' if args.live else 'read-only','model':'dfmodel','maxModelRequests':4 if args.live else 0,'maxOutputTokensPerRequest':128,'checks':[]}
def request(path,body=None):
 req=urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers=headers)
 try:
  with urllib.request.urlopen(req,timeout=185) as f:return f.status,f.headers.get('content-type',''),f.read().decode()
 except urllib.error.HTTPError as e:return e.code,e.headers.get('content-type',''),e.read().decode()
def event_summary(wire):
 events=[];errors=[]
 for line in wire.splitlines():
  if line.startswith('event:'):events.append(line[6:].strip())
  if line.startswith('data:'):
   try:
    v=json.loads(line[5:])
    if v.get('type') in ['error','response.failed']:errors.append(v.get('error') or v.get('response',{}).get('error'))
   except (ValueError,TypeError):pass
 return {'eventTypes':list(dict.fromkeys(events)),'errors':errors}
def check(name,passed,**fields):
 row={'name':name,'passed':bool(passed),**fields};report['checks'].append(row);print(json.dumps(row,ensure_ascii=False))
status,_,body=request('/v1/models');models=json.loads(body).get('data',[]);check('models',status==200,count=len(models))
status,_,body=request('/v1/messages/count_tokens',{'model':'dfmodel','messages':[{'role':'user','content':'local token count fixture'}]});check('count_tokens',status==200 and json.loads(body).get('input_tokens',0)>0)
if args.live:
 tool={'name':'echo_probe','description':'Echo the probe string exactly once.','input_schema':{'type':'object','properties':{'value':{'type':'string'}},'required':['value'],'additionalProperties':False}}
 messages=[{'role':'user','content':'Call echo_probe once with value PROBE. After receiving its result, reply OK.'}]
 body={'model':'dfmodel','max_tokens':128,'thinking':{'type':'disabled'},'messages':messages,'tools':[tool],'tool_choice':{'type':'any'}}
 status,_,raw=request('/v1/messages',body);v=json.loads(raw);calls=[x for x in v.get('content',[]) if x.get('type')=='tool_use'];check('messages_tool',status==200 and bool(calls),http=status,stopReason=v.get('stop_reason'),usage=v.get('usage'))
 if calls:
  body['messages']=messages+[{'role':'assistant','content':v['content']},{'role':'user','content':[{'type':'tool_result','tool_use_id':calls[0]['id'],'content':'PROBE'}]}];body.pop('tool_choice');body['stream']=True
  status,_,wire=request('/v1/messages',body);check('messages_tool_result_sse',status==200 and 'event: message_stop' in wire and 'event: error' not in wire,http=status,**event_summary(wire))
 custom={'type':'custom','name':'echo_probe','description':'Return the exact raw string PROBE once.','format':{'type':'text'}}
 first={'model':'dfmodel','max_output_tokens':128,'reasoning':{'effort':'none'},'input':'Call echo_probe with the raw string PROBE once. Then after its result reply OK.','tools':[custom],'tool_choice':{'type':'custom','name':'echo_probe'}}
 status,_,raw=request('/v1/responses',first);v=json.loads(raw);calls=[x for x in v.get('output',[]) if x.get('type')=='custom_tool_call'];check('responses_custom_tool',status==200 and bool(calls),http=status,responseStatus=v.get('status'),usage=v.get('usage'))
 if calls:
  second=dict(first);second.pop('tool_choice');second['input']=[{'role':'user','content':first['input']}]+v['output']+[{'type':'custom_tool_call_output','call_id':calls[0]['call_id'],'output':'PROBE'}];second['stream']=True
  status,_,wire=request('/v1/responses',second);check('responses_tool_result_sse',status==200 and 'event: response.completed' in wire and 'event: response.failed' not in wire,http=status,**event_summary(wire))
folder=Path(__file__).resolve().parent.parent/'docs/evidence';folder.mkdir(parents=True,exist_ok=True);target=folder/'gateway-smoke.json';target.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
if not all(x['passed'] for x in report['checks']):raise SystemExit(1)

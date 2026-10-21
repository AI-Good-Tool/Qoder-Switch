// Load only the installed Qoder runtime's lazy WASM implementation, never its CLI entrypoint.
// No official code or credentials are included in this project or distributable.
import { readFile, mkdir, writeFile, rename } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { pathToFileURL } from 'node:url';

async function load(edition) {
  const app = edition === 'cn' ? 'Qoder CN' : 'Qoder';
  const sdk = edition === 'cn' ? 'qoder-cn-agent-sdk' : 'qoder-agent-sdk';
  const source = `/Applications/${app}.app/Contents/Resources/app.asar.unpacked/node_modules/@qoder-ai/${sdk}/dist/_worker/qoder-worker-runtime.obf.mjs`;
  let code = await readFile(source, 'utf8');
  const entry = 'ok(),Zir(async()=>';
  const at = code.lastIndexOf(entry);
  if (at < code.length - 400 || at < 0 || !code.includes('function pO()') || !code.includes('function Gm()')) {
    throw new Error('当前 Qoder 运行时版本不受支持，请更新适配器');
  }
  const hash = createHash('sha256').update(code).digest('hex').slice(0, 20);
  const dir = join(homedir(), '.qoder-switch', 'runtime-cache');
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const file = join(dir, `${edition}-${hash}-v2.mjs`);
  code = code.slice(0, at).replaceAll('import.meta.url', JSON.stringify(pathToFileURL(source).href));
  code += '\nexport async function wasmOnly(){ZNe();return await pO();}\n';
  try { await readFile(file); } catch {
    const temp = `${file}.${process.pid}.tmp`;
    await writeFile(temp, code, { mode: 0o600 });
    await rename(temp, file);
  }
  const module = await import(pathToFileURL(file).href);
  return await module.wasmOnly();
}

async function main(input) {
  const wasm = await load(input.edition ?? 'intl');
  if (input.op === 'check') return { ok: true, capabilities: ['prepare_models', 'decrypt', 'runtime_auth'] };
  if (input.op === 'decrypt') {
    let text = input.text;
    try { text = wasm.decrypt_server_response(text); } catch { /* Plain JSON is also valid. */ }
    return JSON.parse(text);
  }
  const fields = JSON.parse(wasm.generate_runtime_auth_fields(JSON.stringify({
    uid: input.uid, security_oauth_token: input.token, ...(typeof input.dataPolicyAgreed === 'boolean' ? {data_policy_agreed:input.dataPolicyAgreed} : {}),
  })));
  const ctx = new wasm.QoderContext(input.machineId, '1.1.66', JSON.stringify({
    uid: input.uid, encrypt_user_info: fields.encrypt_user_info, key: fields.key, ...(typeof input.dataPolicyAgreed === 'boolean' ? {data_policy_agreed:input.dataPolicyAgreed} : {}),
  }), JSON.stringify({client_type:'5',business_product:'cli',business_type:'agent',scene:'assistant'}));
  try {
    const result = input.op === 'prepare_chat'
      ? ctx.prepareInferRequest(input.endpoint, JSON.stringify(input.body), input.model, 'system')
      : ctx.prepareRequest(input.endpoint, '/api/v2/model/list?Encode=1', 'GET', 'auth', undefined, undefined);
    try { return { url: result.url, headers: Object.fromEntries(result.headers), ...(input.op === 'prepare_chat' ? {body:result.body} : {}) }; }
    finally { result.free(); }
  } finally { ctx.free(); }
}

let text = '';
for await (const part of process.stdin) {
  text += part;
  if (Buffer.byteLength(text) > 36 * 1024 * 1024) throw new Error('输入超过上限');
}
try {
  const output = await main(JSON.parse(text));
  process.stdout.write(JSON.stringify({ok:true,data:output}));
} catch {
  // Do not echo native exceptions: these can include authentication material.
  process.stdout.write(JSON.stringify({ok:false,error:'Qoder 本地协议运行时不可用，请确认对应版本 Qoder 已安装且与适配器兼容'}));
  process.exitCode = 1;
}

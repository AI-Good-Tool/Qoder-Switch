import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';

const helper = fileURLToPath(new URL('./qoder-runtime.mjs', import.meta.url));
test('load only lazy WASM, never execute native CLI; support paths with spaces', async () => {
  const home = await mkdtemp(join(tmpdir(), 'qoder runtime 测试 '));
  try {
    const source = join(home, 'native runtime.mjs');
    for (const entry of ['ok(),Zir(async()=>', 'fx(),vsr(async()=>']) {
      const code = 'let cache,loading;function boot(){};function S(fn){return fn};let init=S(()=>{boot(),JSON.stringify({uid:"",encrypt_user_info:"",key:""})});async function load(){return cache||loading||("qoder_auth_wasm",Promise.resolve({}))};' + entry + '{throw Error("CLI must not run")});';
      await writeFile(source, code);
      const output = spawnSync(process.execPath, [helper], {
        input: JSON.stringify({op:'check',edition:'cn'}),
        env: {...process.env, HOME:home, USERPROFILE:home, QODER_SWITCH_RUNTIME_SOURCE:source},
        encoding:'utf8', timeout:10000,
      });
      assert.equal(output.status, 0, output.stdout + output.stderr);
      assert.equal(JSON.parse(output.stdout).data.ok, true);
    }
    await writeFile(source, 'throw Error("synthetic-secret-must-not-leak")');
    const output = spawnSync(process.execPath, [helper], {
      input:'{"op":"check"}',
      env:{...process.env, HOME:home, USERPROFILE:home, QODER_SWITCH_RUNTIME_SOURCE:source},
      encoding:'utf8', timeout:10000,
    });
    assert.equal(output.status, 1);
    assert.equal(JSON.parse(output.stdout).ok, false);
    assert.ok(!output.stdout.includes('synthetic-secret'));
  } finally { await rm(home, {recursive:true,force:true}); }
});

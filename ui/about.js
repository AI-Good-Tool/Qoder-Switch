(() => {
  const $=id=>document.getElementById(id), text=(zh,en)=>tr(zh,en);
  const urls={profile:'https://github.com/vs2pk0',repo:'https://github.com/AI-Good-Tool/Qoder-Switch',sponsor:'https://github.com/vs2pk0/codex-switcher/blob/main/doc/sponsor.md',feedback:'https://github.com/AI-Good-Tool/Qoder-Switch/issues',releases:'https://github.com/AI-Good-Tool/Qoder-Switch/releases'};
  let info=null, file=null, busy=false, installing=false, target='';
  function status(message,error=false) {
    $('aboutUpdateStatus').textContent=message;
    $('aboutUpdate').classList.toggle('has-error',error);
  }
  function buttons() {
    $('aboutCheckButton').disabled=busy||installing;
    document.querySelectorAll('[data-about-action="check"].about-tile').forEach(el=>el.disabled=busy||installing);
    $('aboutDownloadButton').hidden=!info?.canDownload||!!file;
    $('aboutDownloadButton').disabled=busy||installing;
    $('aboutCancelButton').hidden=!busy||$('aboutUpdateProgress').hidden||installing;
    $('aboutInstallButton').hidden=!file;
    $('aboutInstallButton').disabled=busy||installing;
    $('aboutAutoCheck').disabled=installing;
  }
  async function check(silent=false) {
    if(busy||installing)return;
    busy=true;info=null;file=null;$('aboutInstallConfirm').hidden=true;$('aboutReleaseNotes').hidden=true;$('aboutUpdateProgress').hidden=true;
    status(text('正在检查最新版本…','Checking for updates…'));buttons();
    try {
      info=await invoke('app_update_check');
      target=info.target;
      $('aboutUpdateTarget').textContent=`${info.target} · ${text('当前版本','Current')} v${info.currentVersion}`;
      status(info.hasUpdate ? (info.canDownload?text(`发现新版本 v${info.latestVersion}`,`Version ${info.latestVersion} is available`):text(`发现 v${info.latestVersion}，当前平台安装包尚未就绪，请稍后重试。此前可在发布页查看。 `,`Version ${info.latestVersion} found; this platform's installer is not ready. Try again later.`)) : text('当前已是最新版本','You’re up to date'));
      if(info.hasUpdate&&info.releaseNotes) {$('aboutReleaseNotes').textContent=info.releaseNotes;$('aboutReleaseNotes').hidden=false;}
    } catch(error) {status(text('检查更新失败：','Could not check for updates: ')+String(error),true);if(!silent)toast(text('检查失败，请稍后重试或打开发布页','Check failed. Try again or visit Releases'),false);}
    finally{busy=false;buttons();}
  }
  async function download() {
    if(busy||installing||!info?.canDownload)return;
    busy=true;file=null;$('aboutUpdateProgress').hidden=false;$('aboutProgress').removeAttribute('value');$('aboutProgressText').textContent=text('正在连接下载服务器…','Connecting…');
    status(text('正在下载更新，请保持应用打开','Downloading. Keep the app open'));buttons();
    try{file=await invoke('app_update_download');status(text(`v${file.version} 已下载并校验，可以安装。`,`Version ${file.version} downloaded and verified. Ready to install.`));$('aboutProgress').value=100;$('aboutProgressText').textContent=text('SHA-256 校验通过','SHA-256 verified');}
    catch(error){status(String(error),String(error)!=='下载已取消');$('aboutUpdateProgress').hidden=true;}
    finally{busy=false;buttons();}
  }
  document.addEventListener('click',async event=>{
    const button=event.target.closest('[data-about-action]');if(!button)return;
    const action=button.dataset.aboutAction;
    try{
      if(urls[action]){await invoke('open_url',{url:urls[action]});return;}
      if(action==='check'){await check();return;}
      if(action==='download'){await download();return;}
      if(action==='cancel-download'){await invoke('app_update_cancel_download');status(text('正在取消下载…','Cancelling…'));return;}
      if(action==='install-cancel'){$('aboutInstallConfirm').hidden=true;return;}
      if(action==='install'&&file){
        $('aboutInstallConfirmText').textContent=target.startsWith('macos')?text('安装将退出应用并中断正在进行的反代请求。校验安装包后替换应用并重启，失败时保留旧版备份。','Installation quits the app and interrupts active proxy requests. The app restarts after replacement; a backup is kept on failure.'):text('安装将退出应用并中断正在进行的反代请求，自动更新当前安装目录并重启应用，无需操作安装向导。Windows 可能要求授权。','Installation quits the app and interrupts active proxy requests, updates the current installation automatically and restarts the app without a setup wizard. Windows may ask for permission.');
        $('aboutInstallConfirm').hidden=false;return;
      }
      if(action==='install-go'&&file&&!busy&&!installing){
        installing=true;buttons();$('aboutInstallConfirm').hidden=true;status(text('正在校验并准备安装…','Verifying and preparing installation…'));
        try{await invoke('app_update_install_and_restart',{path:file.path});}catch(error){installing=false;status(String(error),true);buttons();}
      }
    }catch(error){toast(String(error),false);}
  });
  $('aboutAutoCheck').addEventListener('change',async event=>{
    const value=event.target.checked;
    try{await invoke('set_setting',{key:'autoCheckUpdates',value});}
    catch(error){event.target.checked=!value;toast(String(error),false);}
  });
  async function initialize() {
    try {
      const meta=await invoke('app_update_metadata');
      $('aboutVersion').textContent='v'+meta.version;
      target=meta.target;$('aboutUpdateTarget').textContent=target;
      document.querySelector('#sidebar .foot b').textContent=meta.version;
      $('aboutAutoCheck').checked=meta.autoCheckUpdates;
      if(window.__TAURI__.event?.listen)await window.__TAURI__.event.listen('qoder-switch-app-update-download-progress',({payload:p})=>{
        if(p.status==='installing'){status(p.message||text('正在准备安装…','Preparing installation…'));return;}
        if(!busy)return;
        if(p.totalBytes){$('aboutProgress').value=Math.min(100,p.downloadedBytes/p.totalBytes*100);}else{$('aboutProgress').removeAttribute('value');}
        $('aboutProgressText').textContent=`${fmtBytes(p.downloadedBytes)}${p.totalBytes?' / '+fmtBytes(p.totalBytes):''}`;
      });
      const error=await invoke('app_update_take_install_error');
      if(error){status(error,true);document.querySelector('[data-page="about"]').click();}
      else if(meta.autoCheckUpdates)setTimeout(()=>check(true),1500);
    }catch(error){status(text('无法初始化更新功能：','Could not initialize updates: ')+String(error),true);}
    buttons();
  }
  initialize();
})();

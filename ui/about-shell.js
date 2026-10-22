(() => {
  const label=(zh,en)=>`<span data-zh="${zh}" data-en="${en}">${zh}</span>`;
  const icon=(path)=>`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${path}</svg>`;
  const paths={
    profile:'<circle cx="12" cy="8" r="4"/><path d="M5 21v-2a7 7 0 0 1 14 0v2z"/>',
    repo:'<path d="m8 7-5 5 5 5m8-10 5 5-5 5m-3-14-2 18"/>',
    sponsor:'<path d="M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1.1-1.1a5.5 5.5 0 0 0-7.8 7.8L12 21l8.8-8.6a5.5 5.5 0 0 0 0-7.8z"/>',
    feedback:'<path d="M21 11.5a8.5 8.5 0 0 1-8.5 8.5H3v-9a9 9 0 0 1 18 .5zM8 9h8M8 13h5"/>',
    check:'<path d="M20 7v5h-5M4 17v-5h5"/><path d="M6 7a7 7 0 0 1 12-2l2 3M4 16l2 3a7 7 0 0 0 12-2"/>'
  };
  document.querySelector('#sidebar .foot').insertAdjacentHTML('beforebegin',`<div class="nav-item" role="button" tabindex="0" data-page="about">${icon('<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7h.01"/>')}${label('关于','About')}</div>`);
  const nav=document.querySelector('[data-page="about"]');
  nav.addEventListener('keydown',e=>{if(e.key==='Enter'||e.key===' '){e.preventDefault();nav.click();}});
  const tiles=[
    ['profile','作者主页','Author','访问 vs2pk0 的 GitHub 主页','Visit vs2pk0 on GitHub'],
    ['repo','开源仓库','Repository','查看源码、版本和发布记录','Source code, versions and releases'],
    ['sponsor','赞助支持','Sponsor','查看作者的赞助支持页面','Support the author'],
    ['feedback','问题反馈','Feedback','提交 Issue 或改进建议','Report an issue or suggest improvements'],
    ['check','检查更新','Check for updates','获取最新版本，在应用内下载安装','Download and install the latest version']
  ];
  document.getElementById('main').insertAdjacentHTML('beforeend',`
    <section class="page gateway-page about-page" id="page-about">
      <div class="about-hero">
        <img class="about-icon" src="app-icon.png" alt="Qoder Switch" width="96" height="96">
        <div class="about-kicker">${label('本地 Qoder 管理工具','YOUR LOCAL QODER WORKSPACE')}</div>
        <h1>Qoder Switch</h1>
        <div class="about-badges"><span id="aboutVersion">—</span><button data-about-action="repo">GitHub ↗</button></div>
        <p>${label('把账号切换、会话管理、用量统计和本地 API 服务，收在一个桌面工作台里。','Account switching, sessions, usage and your local API, together in one workspace.')}</p>
      </div>
      <div class="about-grid">${tiles.map(([id,zh,en,subZh,subEn])=>`<button type="button" class="about-tile" data-about-action="${id}"><span class="about-tile-icon about-${id}">${icon(paths[id])}</span><span><strong>${label(zh,en)}</strong><small>${label(subZh,subEn)}</small></span><span class="about-arrow" aria-hidden="true">↗</span></button>`).join('')}</div>
      <section class="about-update" id="aboutUpdate" aria-label="Application update">
        <div class="about-update-head"><div><h2>${label('应用更新','Application updates')}</h2><p id="aboutUpdateTarget">GitHub Releases</p></div><label class="about-auto"><input type="checkbox" id="aboutAutoCheck" checked>${label('启动时检查更新','Check on startup')}</label></div>
        <div class="about-update-state" role="status" aria-live="polite"><span class="about-update-dot"></span><span id="aboutUpdateStatus">${label('检查是否有新版本可用','Check for a newer version')}</span></div>
        <div id="aboutUpdateProgress" hidden><progress id="aboutProgress" max="100" value="0"></progress><small id="aboutProgressText"></small></div>
        <pre id="aboutReleaseNotes" hidden></pre>
        <div class="about-update-actions"><button type="button" data-about-action="check" id="aboutCheckButton">${label('检查更新','Check for updates')}</button><button type="button" class="primary" data-about-action="download" id="aboutDownloadButton" hidden>${label('下载更新','Download update')}</button><button type="button" data-about-action="cancel-download" id="aboutCancelButton" hidden>${label('取消下载','Cancel download')}</button><button type="button" class="primary" data-about-action="install" id="aboutInstallButton" hidden>${label('安装更新','Install update')}</button><button type="button" data-about-action="releases">${label('查看发布页','View releases')} ↗</button></div>
        <div class="about-install-confirm" id="aboutInstallConfirm" hidden><p id="aboutInstallConfirmText"></p><button class="primary" type="button" data-about-action="install-go">${label('确认退出并安装','Quit and install')}</button><button type="button" data-about-action="install-cancel">${label('暂不安装','Not now')}</button></div>
      </section>
      <p class="about-footer">Qoder Switch · ${label('开源 · 本地运行','Open source · Runs locally')}</p>
    </section>`);
})();

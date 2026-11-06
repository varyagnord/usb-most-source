'use strict';
(() => {
  let token = location.hash.slice(1);
  history.replaceState(null, '', '/');
  const el = id => document.getElementById(id);
  let busy = false, imageUrl = null, storeImageUrl = null, expiry = 0, timer = null;
  let stage = 'download', ready = false, visible = true, poll = null, peerSignature = null;
  async function api(action) {
    const response = await fetch('/api', {method: 'POST', credentials: 'omit', cache: 'no-store', headers: {'Content-Type': 'application/json', 'X-USBMost-Session': token}, body: JSON.stringify(action)});
    if (response.status === 403) { visible = false; clearTimeout(poll); clearCode(); token = ''; throw Error('Сеанс настройки завершён. Откройте USB Мост на компьютере снова.'); }
    const data = await response.json();
    if (!response.ok) throw Error(data.error || 'Действие не завершено. Повторите попытку.');
    return data;
  }
  function clearCode() {
    clearInterval(timer); timer = null; el('claim').hidden = true; el('code').value = ''; el('qr').removeAttribute('src');
    if (imageUrl) URL.revokeObjectURL(imageUrl); imageUrl = null;
  }
  function renderStage() {
    el('download').hidden = !ready || stage !== 'download';
    el('pairing').hidden = !ready || stage !== 'pairing';
  }
  async function action(work, quiet = false) {
    if (busy || !visible) return; busy = true;
    if (!quiet) el('error').textContent = '';
    document.querySelectorAll('button').forEach(b => b.disabled = true);
    try { await work(); } catch (error) { el('error').textContent = error.message; }
    finally { busy = false; document.querySelectorAll('button').forEach(b => b.disabled = !visible); }
  }
  async function download() {
    const info = await api({op:'download'});
    const available = typeof info.url === 'string';
    el('store-available').hidden = !available; el('store-pending').hidden = available;
    if (available) {
      // Only a public listing URL comes from the build-owned API. Pairing secrets
      // and this page's local owner capability never enter a store request.
      storeImageUrl = URL.createObjectURL(new Blob([info.svg], {type:'image/svg+xml'}));
      el('store-qr').src = storeImageUrl; el('store-link').href = info.url;
    }
  }
  async function refresh() {
    const state = await api({op: 'status'});
    if (!visible) return;
    const wasReady = ready;
    ready = state.running && state.direct && state.stack;
    el('status').textContent = !state.running ? 'Компьютер ещё не подготовлен' : !state.stack ? 'Не удалось подготовить USB. Повторите настройку.' : state.connected ? `Android-устройство подключено · USB-устройств: ${state.active}` : 'Компьютер готов к подключению';
    el('prepare').hidden = ready;
    if (wasReady && !ready) { clearCode(); stage = 'download'; }
    if (state.connected) clearCode();
    renderStage();
    el('access').hidden = !state.direct;
    el('addresses').replaceChildren();
    for (const address of state.addresses) { const option = document.createElement('option'); option.value = address; el('addresses').append(option); }
    if (!el('address').value && state.addresses.length) el('address').value = state.addresses[0];
    const signature = JSON.stringify(state.peers);
    if (signature === peerSignature) return;
    peerSignature = signature;
    el('peers').replaceChildren();
    for (const [id, name] of state.peers) {
      const row = document.createElement('li'), label = document.createElement('span'), button = document.createElement('button');
      label.textContent = name; button.textContent = 'Отозвать доступ'; button.className = 'secondary';
      button.addEventListener('click', () => action(async () => { await api({op:'revoke', id}); clearCode(); await refresh(); }));
      row.append(label, button); el('peers').append(row);
    }
    if (!state.peers.length) { const row = document.createElement('li'); row.textContent = 'Доступ ещё никому не выдан'; el('peers').append(row); }
  }
  async function pair() {
    clearCode();
    const host = el('address').value.trim();
    if (!host) { el('network').open = true; el('address').focus(); throw Error('Укажите адрес компьютера в сети, доступной Android-устройству.'); }
    const claim = await api({op: 'pair', host});
    if (!visible) return;
    imageUrl = URL.createObjectURL(new Blob([claim.svg], {type: 'image/svg+xml'})); el('qr').src = imageUrl; el('code').value = claim.code;
    expiry = performance.now() + claim.expires_seconds * 1000; el('claim').hidden = false;
    const tick = () => { const seconds = Math.max(0, Math.ceil((expiry - performance.now()) / 1000)); if (!seconds) { clearCode(); el('error').textContent = 'Срок действия кода истёк. Получите новый код.'; return; } el('expires').textContent = `Код действует ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`; };
    tick(); timer = setInterval(tick, 1000);
  }
  el('install').addEventListener('click', () => action(async () => { el('status').textContent = 'Подготавливаем компьютер…'; await api({op: 'install'}); await refresh(); el('download-title').focus(); }));
  el('app-ready').addEventListener('click', () => action(async () => { stage = 'pairing'; renderStage(); el('pairing-title').focus(); await pair(); }));
  el('back').addEventListener('click', () => { clearCode(); stage = 'download'; renderStage(); el('download-title').focus(); });
  el('pair').addEventListener('click', () => action(pair));
  el('copy').addEventListener('click', () => action(async () => { await navigator.clipboard.writeText(el('code').value); el('expires').textContent = 'Код скопирован'; }));
  el('address').addEventListener('input', clearCode);
  window.addEventListener('pagehide', () => { visible = false; clearTimeout(poll); clearCode(); token = ''; if (storeImageUrl) URL.revokeObjectURL(storeImageUrl); });
  async function schedule() {
    if (!visible) return;
    if (!document.hidden) await action(refresh, true);
    if (visible) poll = setTimeout(schedule, 3000);
  }
  if (!/^[A-Za-z0-9_-]{43}$/.test(token)) { el('error').textContent = 'Откройте ссылку, выданную USB Мост на этом компьютере.'; el('status').textContent = 'Для настройки нужен локальный доступ'; return; }
  action(async () => { await download(); await refresh(); }).then(() => { if (visible) poll = setTimeout(schedule, 3000); });
})();

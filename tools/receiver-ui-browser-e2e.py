#!/usr/bin/env python3
"""Real local HTTP/owner IPC wizard; isolated fixtures and intercepted store navigation."""
import json, pathlib, sys
from playwright.sync_api import sync_playwright
fixture = json.load(sys.stdin)
assert fixture['scope'] == 'temporary-owner-ipc-no-usb'
url, listing = fixture['url'], fixture['listing']
assert url.startswith('http://127.0.0.1:') and '/#' in url
root = pathlib.Path(__file__).resolve().parents[1]
out = root / 'build/e2e/onboarding111' / ('configured' if listing else 'pending')
out.mkdir(parents=True, exist_ok=True, mode=0o700)
errors, external, store_requests = [], [], []
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True)
    context = browser.new_context(locale='ru-RU', permissions=['clipboard-read', 'clipboard-write'])
    def store_route(route):
        store_requests.append({'url':route.request.url,'referer':route.request.headers.get('referer')})
        route.fulfill(status=200, content_type='text/html', body='<title>Offline store fixture</title>')
    context.route('https://www.rustore.ru/**', store_route)
    page = context.new_page()
    page.clock.install()
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.on('request', lambda request: external.append(request.url) if not request.url.startswith(('http://127.0.0.1:', 'blob:')) else None)
    page.goto(url)
    page.locator('#download').wait_for(state='visible')
    assert page.url.endswith('/') and '#' not in page.url
    assert not page.locator('#pairing').is_visible()
    assert not page.locator('#claim').is_visible()
    assert '<script>fixture</script>' in page.locator('#peers').inner_text()
    assert page.locator('#peers script').count() == 0
    if listing:
        page.locator('#store-qr').wait_for(state='visible')
        assert page.locator('#store-link').get_attribute('href') == listing
        page.locator('#store-qr').screenshot(path=str(out / 'store-rendered.png'))
        assert not page.locator('#store-pending').is_visible()
        assert not external and not store_requests, 'no store contact before explicit action'
        with page.expect_popup() as opened:
            page.locator('#store-link').click()
        opened.value.wait_for_load_state()
        assert opened.value.url == listing
        assert store_requests == [{'url':listing,'referer':None}]
        opened.value.close()
    else:
        assert page.locator('#store-pending').is_visible()
        assert not page.locator('#store-qr').is_visible()
        assert page.locator('#store-link').get_attribute('href') is None
        assert not store_requests
    # The download QR never creates or exposes a pairing claim.
    assert page.locator('#code').input_value() == ''
    page.locator('#app-ready').click()
    page.locator('#pairing').wait_for(state='visible')
    page.locator('#qr').wait_for(state='visible')
    assert page.locator('#code').input_value().startswith('usbmost:1:')
    page.locator('#back').click()
    assert page.locator('#download').is_visible() and not page.locator('#claim').is_visible()
    assert page.locator('#code').input_value() == ''
    page.locator('#app-ready').click()
    page.locator('#claim').wait_for(state='visible')
    for width in [320, 360, 720, 1280]:
        page.set_viewport_size({'width': width, 'height': 900})
        page.locator('#network').evaluate('(node) => node.open = true')
        page.locator('#address').fill('192.0.2.42')
        page.locator('#pair').click()
        page.locator('#qr').wait_for(state='visible')
        page.wait_for_function('document.querySelector("#qr").naturalWidth > 0')
        assert page.locator('#code').input_value().startswith('usbmost:1:')
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
        assert page.locator('#pair').bounding_box()['height'] >= 48
        page.screenshot(path=str(out / f'pairing-width-{width}.png'), full_page=True)
        page.locator('#address').fill('192.0.2.43')
        assert not page.locator('#claim').is_visible()
        page.locator('#back').click()
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
        page.screenshot(path=str(out / f'download-width-{width}.png'), full_page=True)
        page.locator('#app-ready').click()
        page.locator('#claim').wait_for(state='visible')
    page.set_viewport_size({'width': 360, 'height': 900})
    page.evaluate("document.styleSheets[0].insertRule(':root {font-size: 200% !important}', 0)")
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.screenshot(path=str(out / 'font-200.png'), full_page=True)
    page.locator('#qr').screenshot(path=str(out / 'pairing-rendered.png'))
    expected = out / 'pairing.txt'
    expected.write_text(page.locator('#code').input_value()); expected.chmod(0o600)
    page.clock.fast_forward(301_000)
    page.locator('#error').filter(has_text='Срок действия кода истёк').wait_for()
    assert not page.locator('#claim').is_visible() and page.locator('#code').input_value() == ''
    page.locator('#pair').click()
    page.locator('#claim').wait_for(state='visible')
    page.locator('#claim summary').click()
    page.locator('#copy').click()
    page.locator('#expires').filter(has_text='Код скопирован').wait_for()
    assert page.evaluate('navigator.clipboard.readText()') == page.locator('#code').input_value()
    page.locator('#address').fill('https://bad.example')
    page.locator('#pair').click()
    page.locator('#error').filter(has_text='Действие не завершено').wait_for()
    assert not page.locator('#claim').is_visible()
    # UI failure paths use an intercepted API, never the real OS installer.
    mock = {'ready':False,'installs':0,'deny':False,'pairs':0,'pair_ok':False,'connected':False}
    simulated = context.new_page()
    simulated.clock.install()
    def simulated_api(route):
        data = route.request.post_data_json
        if mock['deny']:
            route.fulfill(status=403,content_type='application/json',body='{}'); return
        if data['op']=='download': result = {'url':None,'svg':None}
        elif data['op']=='status': result = {'running':mock['ready'],'direct':mock['ready'],'stack':mock['ready'],'connected':mock['connected'],'active':0,'addresses':['192.0.2.42'],'peers':[]}
        elif data['op']=='install':
            mock['installs'] += 1
            if mock['installs']==1:
                route.fulfill(status=400,content_type='application/json',body=json.dumps({'error':'Нужны права администратора.'})); return
            mock['ready']=True; result={'ok':True}
        elif data['op']=='pair':
            mock['pairs'] += 1
            if mock['pair_ok']:
                route.continue_(); return
            route.fulfill(status=400,content_type='application/json',body=json.dumps({'error':'Действие не завершено. Повторите попытку.'})); return
        else: raise AssertionError(data)
        route.fulfill(status=200,content_type='application/json',body=json.dumps(result))
    simulated.route('**/api',simulated_api)
    simulated.goto(url)
    simulated.locator('#prepare').wait_for(state='visible')
    assert not simulated.locator('#download').is_visible()
    simulated.locator('#install').click()
    simulated.locator('#error').filter(has_text='Нужны права администратора').wait_for()
    assert not simulated.locator('#download').is_visible() and mock['installs']==1
    simulated.locator('#install').click()
    simulated.locator('#download').wait_for(state='visible')
    assert mock['installs']==2 and mock['pairs']==0
    simulated.locator('#app-ready').click()
    simulated.locator('#error').filter(has_text='Действие не завершено').wait_for()
    assert not simulated.locator('#claim').is_visible()
    mock['pair_ok']=True
    simulated.locator('#pair').click()
    simulated.locator('#claim').wait_for(state='visible')
    mock['connected']=True
    simulated.clock.fast_forward(4000)
    simulated.locator('#status').filter(has_text='Android-устройство подключено').wait_for()
    assert not simulated.locator('#claim').is_visible() and simulated.locator('#code').input_value()==''
    mock['ready']=False
    simulated.clock.fast_forward(4000)
    simulated.locator('#prepare').wait_for(state='visible')
    assert not simulated.locator('#pairing').is_visible()
    mock['deny']=True
    simulated.clock.fast_forward(4000)
    simulated.locator('#error').filter(has_text='Сеанс настройки завершён').wait_for()
    assert simulated.locator('#install').is_disabled()
    simulated.close()
    other = context.new_page()
    other.goto(url.split('#')[0])
    other.locator('#error').filter(has_text='Откройте ссылку').wait_for()
    assert not other.locator('#pairing').is_visible() and not other.locator('#download').is_visible()
    assert not errors, errors
    assert not external, external
    context.close(); browser.close()
(out / 'result.json').write_text(json.dumps({'status':'PASS','listing_configured':bool(listing), 'widths':[320,360,720,1280], 'font_percent':200,'store_navigation':'intercepted offline; only after explicit click; no Referrer', 'scope':'real browser -> local HTTP -> owner IPC; installation link and synthetic pairing; no real RuStore install or physical USB'}, indent=2)+'\n')
print('PASS wizard: pending/configured listing, separate QR, back/expiry/retry, clipboard, responsive, no automatic external requests')

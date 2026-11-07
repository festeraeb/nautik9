"""Playwright checks for the God's Eye tab (chooser with the probe mocked both ways, thresholds as settings,
tracking switch behaviour, the tab in all three themes). Needs: a running nautik9-app (NAUTIK9_URL, default
http://127.0.0.1:18777) whose config dir holds `token`, a godseye.json pointing at a reachable track service and
stream, and that service's loopback URL (TRACK_LOOPBACK, default http://127.0.0.1:4175) for reading the fixes back.
  NAUTIK9_CONFIG=/path/to/config /path/to/python crates/nautik9-app/tests/godseye-e2e.py
The probe page is replaced by a stub that posts chosen numbers, so no GPU is needed; no value here is a measurement."""
import os, json, re
from playwright.sync_api import sync_playwright
CFG=os.environ['NAUTIK9_CONFIG']; OUT=os.environ.get('SHOTS','/tmp'); URL=os.environ.get('NAUTIK9_URL','http://127.0.0.1:18777'); TRACK=os.environ.get('TRACK_LOOPBACK','http://127.0.0.1:4175')
tok=open(CFG+'/token').read().strip(); errs=[]; results=[]
def check(name, ok, detail=''): results.append((name,bool(ok),detail)); print(('PASS ' if ok else 'FAIL ')+name+(' - '+str(detail) if detail else ''))
def probe_page(fps, ok=True):
    body = json.dumps({'type':'godseye-probe','ok':ok,'fps':fps,'medianMs':round(1000/fps,2) if fps else 0,'p95Ms':40,'frames':120,'ms':2000,'webgl2':True,'maxTexture':16384,'renderer':'mock'}) if ok else json.dumps({'type':'godseye-probe','ok':False,'error':'mocked failure'})
    return '<!doctype html><script>parent.postMessage(%s,"*")</script>' % body
INIT = lambda mem, cores, theme='dark': ("localStorage.setItem('nautik9_token','%s');localStorage.setItem('nautik9_theme','%s');" % (tok, theme)) + ("Object.defineProperty(navigator,'deviceMemory',{get:()=>%s});" % mem) + ("Object.defineProperty(navigator,'hardwareConcurrency',{get:()=>%s});" % cores)
def open_tab(b, fps, mem=8, cores=8, theme='dark', mode=None, ok=True, extra=None):
    ctx=b.new_context(viewport={'width':480,'height':1040}, device_scale_factor=1.3, permissions=['geolocation'], geolocation={'latitude':41.8789,'longitude':-87.6359,'accuracy':12})
    ctx.add_init_script(INIT(mem,cores,theme))
    pg=ctx.new_page(); pg.on('pageerror',lambda e: errs.append(str(e)))
    pg.route(re.compile(r'.*:4176/globe\?probe=1.*'), lambda r: r.fulfill(body=probe_page(fps,ok), content_type='text/html'))
    pg.goto(URL+'/'); pg.wait_for_timeout(500)
    if mode: pg.evaluate("fetch('/api/godseye/settings',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mode:'%s'})})"%mode); pg.wait_for_timeout(300)
    pg.evaluate("switchTab('godseye',[...document.querySelectorAll('.tab-btn')].find(b=>b.innerText.includes('Eye')))")
    pg.wait_for_function("document.getElementById('geNumbers').innerText.includes('Globe probe') && !document.getElementById('geReasons').innerText.includes('Measuring')", timeout=30000)
    pg.wait_for_timeout(500)
    return ctx, pg
def decision(pg): return pg.inner_text('#geDecision') + ' | ' + pg.inner_text('#geReasons')
with sync_playwright() as p:
    b=p.chromium.launch(executable_path='/usr/bin/google-chrome',headless=True,args=['--no-sandbox','--use-angle=swiftshader','--enable-unsafe-swiftshader','--ignore-gpu-blocklist'])
    # reset settings
    c0,pg=open_tab(b, 60); pg.evaluate("fetch('/api/godseye/settings',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mode:'auto',thresholds:{min_fps:30,min_memory_gb:4,min_cores:6,allow_save_data:false}})})"); c0.close()
    c,pg=open_tab(b, 60); d=decision(pg); check('fast probe, good phone -> LOCAL', 'Chosen: On this phone' in d and 'clears every threshold' in d, d); 
    check('local view loads the globe iframe', 'globe' in (pg.get_attribute('#geFrame','src') or '') and pg.is_visible('#geFrame'), pg.get_attribute('#geFrame','src')); c.close()
    c,pg=open_tab(b, 8); d=decision(pg); check('slow probe -> STREAM with the number', 'Chosen: Home GPU stream' in d and 'drew 8 frames per second' in d, d)
    check('stream view is the view-only noVNC page', 'view_only=true' in (pg.get_attribute('#geFrame','src') or ''), ''); c.close()
    c,pg=open_tab(b, 60, mem=2); d=decision(pg); check('2 GB phone -> STREAM before any probe', 'Home GPU stream' in d and 'memory 2 GB' in d, d); c.close()
    c,pg=open_tab(b, 60, cores=4); d=decision(pg); check('4 cores -> STREAM', 'Home GPU stream' in d and '4 CPU cores' in d, d); c.close()
    c,pg=open_tab(b, 60, ok=False); d=decision(pg); check('probe failure -> STREAM and says why', 'Home GPU stream' in d and 'mocked failure' in d, d); c.close()
    c,pg=open_tab(b, 8, mode='local'); d=decision(pg); check('forced local overrides, and still says what Auto would pick', 'Mode set by you: On this phone' in d and 'Auto would choose the home GPU' in d, d); c.close()
    # thresholds are run-time settings: lower min_fps below the mocked 8 fps
    c,pg=open_tab(b, 8, mode='auto'); pg.evaluate("fetch('/api/godseye/settings',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({thresholds:{min_fps:5}})})"); pg.wait_for_timeout(300); pg.click('text=Measure again'); pg.wait_for_function("document.getElementById('geDecision').innerText.includes('On this phone')", timeout=20000)
    check('changing the fps threshold in settings changes the decision', True, decision(pg)); pg.evaluate("fetch('/api/godseye/settings',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({thresholds:{min_fps:30}})})"); c.close()
    # tracking
    c,pg=open_tab(b, 60)
    check('track is off by default', not pg.is_checked('#geTrack'), pg.inner_text('#geTrackLabel'))
    pg.check('#geTrack'); pg.wait_for_timeout(2500)
    st=pg.inner_text('#geTrackStatus'); check('toggle on sends a fix over the tailnet door', st.startswith('Sent to home over Tailscale'), st)
    import urllib.request
    j=json.load(urllib.request.urlopen(TRACK+'/api/track/latest?n=3')); fx=j['devices'][0]['fixes'][-1] if j['devices'] else None
    check('the track service has the fix', fx and abs(fx['lat']-41.8789)<1e-4, json.dumps(fx)[:120])
    pg.evaluate("switchTab('helm',document.querySelectorAll('.tab-btn')[0])"); pg.wait_for_timeout(500)
    n0=len(json.load(urllib.request.urlopen(TRACK+'/api/track/latest?n=500'))['devices'][0]['fixes']); pg.wait_for_timeout(9000)
    n1=len(json.load(urllib.request.urlopen(TRACK+'/api/track/latest?n=500'))['devices'][0]['fixes'])
    check('nothing is sent while the tab is not showing', n1==n0, f'{n0}->{n1}')
    pg.evaluate("switchTab('godseye',[...document.querySelectorAll('.tab-btn')].find(b=>b.innerText.includes('Eye')))"); pg.wait_for_timeout(9000)
    n2=len(json.load(urllib.request.urlopen(TRACK+'/api/track/latest?n=500'))['devices'][0]['fixes']); check('sending resumes when the tab shows again', n2>n1, f'{n1}->{n2}')
    pg.uncheck('#geTrack'); pg.wait_for_timeout(500); n3=len(json.load(urllib.request.urlopen(TRACK+'/api/track/latest?n=500'))['devices'][0]['fixes']); pg.wait_for_timeout(9000)
    n4=len(json.load(urllib.request.urlopen(TRACK+'/api/track/latest?n=500'))['devices'][0]['fixes']); check('toggle off stops sending', n4==n3, f'{n3}->{n4}')
    check('no secret or position on the page', tok not in pg.content(), ''); c.close()
    # three themes
    for th in ['dark','lakes','dead']:
        c,pg=open_tab(b, 60, theme=th, mode='auto'); pg.screenshot(path=f'{OUT}/ge-{th}-tab.png', full_page=False)
        sizes=pg.evaluate("[...document.querySelectorAll('#pane-godseye *')].filter(e=>e.scrollWidth>e.clientWidth+2 && getComputedStyle(e).overflowX==='visible' && e.clientWidth>0).length")
        check(f'theme {th}: tab renders with no horizontal overflow', sizes==0, sizes); c.close()
    b.close()
print('page errors:', errs[:3]); print('failed:', [r[0] for r in results if not r[1]])

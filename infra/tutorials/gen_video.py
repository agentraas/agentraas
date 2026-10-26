"""Build one animated tutorial page per tutorial from the recorded cases.
Usage: gen_video.py <base.css.html> <tutorials.json> <outdir>  -> <outdir>/<id>.html + durations.json"""
import html, json, sys

css, data, out = sys.argv[1:4]
BASE = open(css).read()
E = html.escape
EXTRA = """<style>
.scene{padding:84px 160px 130px;gap:28px}
h2{font-size:60px}
.leadS{font-size:30px;line-height:1.4;color:var(--muted)}
.lbl{font-size:22px;font-weight:700;letter-spacing:2px;text-transform:uppercase;color:var(--soft);display:flex;align-items:center;gap:12px}
.cols{display:flex;gap:40px;align-items:flex-start;min-height:0}
.cols>div{flex:1;min-width:0;display:flex;flex-direction:column;gap:14px}
.tbody{padding:22px 30px;font-size:20px;line-height:1.5}
.bar{height:40px}
.res{background:var(--surface);border:1px solid var(--line);border-radius:20px;padding:20px 28px;font-family:'JetBrains Mono',monospace;font-size:20px;line-height:1.5;white-space:pre-wrap;word-break:break-word;color:var(--ink);overflow:hidden}
.res .hl{background:rgba(242,181,68,.3);border-radius:6px;animation:hl 1s ease both;animation-delay:calc((var(--s) + var(--d,0)) * 1s)}
.status{display:inline-block;font-family:'JetBrains Mono',monospace;font-size:22px;font-weight:700;border-radius:8px;padding:2px 12px;letter-spacing:0}
.s2{background:#E3EEF8;color:var(--ok)}.s202{background:#FBF0D9;color:var(--dup)}.s4{background:#FBE4E1;color:var(--bad)}.s5{background:#F1E4E4;color:#7A2E2E}
.why{display:flex;gap:18px;align-items:flex-start;background:var(--ink);color:#ECEFF3;border-radius:18px;padding:22px 28px;font-size:27px;line-height:1.4}
.why i{flex:none;width:12px;height:36px;border-radius:6px;background:var(--amber);margin-top:2px}
.steps{display:flex;flex-direction:column;gap:14px}
.st{display:flex;gap:18px;align-items:baseline;font-size:29px;line-height:1.35}
.st b{flex:none;width:42px;height:42px;border-radius:12px;background:var(--ink);color:#fff;font-size:22px;display:flex;align-items:center;justify-content:center;transform:translateY(8px)}
.term .tbody .l{white-space:pre-wrap;word-break:break-word}
</style>"""

def status_cls(s):
    return 's202' if s == 202 else 's2' if s < 300 else 's5' if s >= 500 else 's4'

KEYS = ('"error"', '"cached"', '"pending_approval"', '"replay"', '"payload_matches"', '"undone"', '"status"', '"hint"', '"result"', '"upstream_id"')

def json_lines(obj, limit=14):
    lines = json.dumps(obj, indent=1).splitlines()
    if len(lines) > limit:
        lines = lines[:limit - 1] + ['  ...']
    return lines

def show_payload(t, p):
    if t['id'] != 'failures':
        p = {k: v for k, v in p.items() if k != 'fail'}
    return p

def request_block(t, c):
    if 'body' in c and c.get('cmd') is None and 'method' not in c:
        hdr = [f'{k}: {v}' for k, v in c['headers'].items()]
        body = {**c['body'], 'payload': show_payload(t, c['body']['payload'])}
        agent = c.get('agent', 'billing-agent')
        return [f'POST /v1/webhook/org_acme/{agent}'] + hdr + [''] + json.dumps(body, indent=1).splitlines()
    lines = [f'{c["method"]} {c["path"]}']
    if c.get('body'): lines += [''] + json.dumps(c['body']).splitlines()
    if c.get('shown'): lines += ['', '# ' + c['shown']]
    return lines

def res_html(lines, s, d0):
    outl = []
    for i, ln in enumerate(lines):
        e = E(ln)
        if any(k in ln for k in KEYS):
            e = f'<span class="hl" style="--d:{d0 + 0.5 + 0.15 * i:.2f}">{e}</span>'
        outl.append(e)
    return '\n'.join(outl)

def build(t):
    scenes, now = [], 0.0
    def scene(dur, body, extra=''):
        nonlocal now
        scenes.append(f'<section class="scene" style="--s:{now:.2f};--e:{now + dur:.2f};{extra}">{body}</section>')
        now += dur
    n = len(t['cases'])
    scene(6, f'''<div class="logo pop" style="--d:0"><i class="ring"></i><i class="disc"></i></div>
<p class="eyebrow a" style="--d:.4">AgentRaaS tutorial</p>
<h1 class="a" style="--d:.5;font-size:100px;letter-spacing:-2px">{E(t["title"])}</h1>
<p class="lead a" style="--d:.9">{E(t["summary"])}</p>
<p class="eyebrow a" style="--d:1.6">How to set it up · {n} cases, each run for real</p>''', 'justify-content:center;gap:30px')
    # setup
    su = t['setup']; items = []
    for i, s in enumerate(su.get('dashboard', [])):
        items.append(f'<div class="st a" style="--d:{.8 + .45 * i:.2f}"><b>{i + 1}</b><span>{E(s)}</span></div>')
    left = ''
    if items:
        left += f'<p class="lbl fade" style="--d:.6">In the dashboard</p><div class="steps">{"".join(items)}</div>'
    if su.get('install'):
        left += f'<p class="lbl fade" style="--d:.6">Install</p><div class="term pop" style="--d:.7"><div class="bar"><b></b><b></b><b></b></div><div class="tbody"><div class="l" style="--d:.9"><span class="p">$ </span>{E(su["install"])}</div></div></div>'
    for k, nt in enumerate(su.get('notes', []) + su.get('env', [])):
        left += f'<p class="leadS a" style="--d:{2.6 + .4 * k:.2f};font-size:26px">{E(nt)}</p>'
    right = ''
    d = 1.6
    for r in su.get('rules', [])[:3]:
        right += f'<p class="lbl fade" style="--d:{d:.2f}">{E(r["label"])}</p><div class="res a" style="--d:{d + .1:.2f};font-size:18px">{E(r["method"] + " " + r["path"])}\n{E(json.dumps(r["body"]))}</div>'
        d += .8
    if su.get('api'):
        a = su['api']
        right += f'<p class="lbl fade" style="--d:{d:.2f}">Or with the API</p><div class="res a" style="--d:{d + .1:.2f};font-size:19px">{E(a["method"] + " " + a["path"])}\n{E(json.dumps(a["body"], indent=1))}</div>'
    if su.get('file'):
        f = su['file']; txt = f['text'].strip().splitlines()[:16]
        right += f'<p class="lbl fade" style="--d:{d:.2f}">{E(f["name"])}</p><div class="res a" style="--d:{d + .1:.2f};font-size:18px">{E(chr(10).join(txt))}</div>'
    scene(9, f'''<div class="head"><p class="eyebrow a" style="--d:.1">How to set it up</p><h2 class="a" style="--d:.2">{E(t["title"])}</h2></div>
<div class="cols"><div>{left}</div><div>{right}</div></div>''')
    for k, c in enumerate(t['cases']):
        if 'cmd' in c:
            out = c['output'].replace('\nscanning billing_server.py ...', '').splitlines()
            out = [l for l in out if not l.startswith('scanning ')][:18]
            body_lines = ''.join(f'<div class="l" style="--d:{1.4 + .05 * i:.2f}">{E(l) or " "}</div>' for i, l in enumerate(out))
            fnote = ''
            dur = 7.5 + min(len(out), 18) * .12
            main = f'''<div class="term pop" style="--d:.6"><div class="bar"><b></b><b></b><b></b></div><div class="tbody">
<div class="l" style="--d:.8"><span class="p">$ </span>{E(c["cmd"])}</div>{body_lines}</div></div>'''
            if c.get('file') and len(c['file']['text'].splitlines()) <= 20:
                main = f'''<div class="cols"><div style="flex:.9"><p class="lbl fade" style="--d:.5">{E(c["file"]["name"])}</p><div class="res a" style="--d:.5;font-size:17px">{E(c["file"]["text"].rstrip())}</div></div><div style="flex:1.1">{main}</div></div>'''
                dur += 2
            body = main
        else:
            req = request_block(t, c)
            resp = json_lines(c['response'])
            dur = 8.5 + len(resp) * .12
            body = f'''<div class="cols">
<div><p class="lbl fade" style="--d:.5">Request</p><div class="term pop" style="--d:.6"><div class="bar"><b></b><b></b><b></b></div><div class="tbody">{"".join(f'<div class="l" style="--d:{.8 + .06 * i:.2f}">{E(l) or " "}</div>' for i, l in enumerate(req))}</div></div></div>
<div><p class="lbl fade" style="--d:2">Response <span class="status {status_cls(c["status"])}">{c["status"]}</span></p><div class="res a" style="--d:2.1">{res_html(resp, c["status"], 2.1)}</div></div>
</div>'''
        why = f'<div class="why a" style="--d:{4.2 if "cmd" not in c else 2.6}"><i></i><span>{E(c["why"])}</span></div>' if c['why'] else ''
        scene(dur, f'''<div class="head"><p class="eyebrow a" style="--d:.1">Case {k + 1} of {n}</p><h2 class="a" style="--d:.2">{E(c["title"])}</h2></div>
{body}{why}''')
    scene(5.6, f'''<div class="logo pop" style="--d:0"><i class="ring"></i><i class="disc"></i></div>
<h2 class="a" style="--d:.3;font-size:84px">{E(t["title"])}</h2>
<div class="row" style="gap:24px"><span class="chip a" style="--d:.8;font-size:30px">agentraas.io/docs#tut-{t["id"]}</span><span class="chip a" style="--d:1;font-size:30px">pip install agentraas</span></div>''', 'justify-content:center;gap:32px')
    total = now
    return f'''<!doctype html><html><head><meta charset="utf-8">
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700;800&family=JetBrains+Mono:wght@400;500;700&display=block" rel="stylesheet">
{BASE}{EXTRA}</head><body style="--total:{total:.2f}">
{"".join(scenes)}
<div id="brand"><span class="mini"><i class="r1"></i><i class="r2"></i></span>AgentRaaS · {E(t["title"])}</div>
<div id="progress"></div>
<script>
document.querySelectorAll('.type').forEach(e => e.style.setProperty('--n', e.textContent.length));
window.seek = t => {{ for (const a of document.getAnimations()) {{ a.pause(); a.currentTime = t * 1000; }} }};
</script></body></html>''', total

durs = {}
for t in json.load(open(data)):
    page, total = build(t)
    open(f'{out}/{t["id"]}.html', 'w').write(page)
    durs[t['id']] = round(total, 2)
json.dump(durs, open(f'{out}/durations.json', 'w'), indent=1)
print(durs, 'total', round(sum(durs.values())))

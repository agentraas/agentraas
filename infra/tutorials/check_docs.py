"""Fail when the tutorials no longer match the server: re-run every server case
(run_server.py) and compare it with tutorials.json, case by case: the status
code, the shape of the response, and its error text (numbers and ids blanked,
they change every run). Values like amounts or timestamps are not compared.

Usage: check_docs.py <creds.txt>, or check_docs.py --signup on a fresh test
server (registers a verified account in org_acme; needs EXPOSE_DEV_VERIFY_URL).
Set AGENTRAAS_URL and REDIS_URL for a server other than this laptop's stack.
When it fails: re-record (README.md) if the change was intended."""
import json, os, re, subprocess, sys, tempfile
from urllib.parse import parse_qs, urlparse
import requests

HERE = os.path.dirname(os.path.abspath(__file__))
B = os.environ.get('AGENTRAAS_URL', 'http://localhost:13000')
TEXT = ('error', 'hint', 'message')

def shape(v):
    if isinstance(v, dict):
        # Days blanked too: the allowed-hours case leaves out the day it runs on.
        return {k: (re.sub(r'[0-9a-f]{8,}|\d+|\b(mon|tue|wed|thu|fri|sat|sun)\b', '#', x) if k in TEXT and isinstance(x, str) else shape(x)) for k, x in v.items()}
    if isinstance(v, list):
        return [shape(v[0])] if v else []
    return type(v).__name__

def signup(path):
    email, pw = 'docs-check@agentraas.test', 'docs-check-password'
    r = requests.post(f'{B}/api/v1/auth/register', json={'email': email, 'password': pw, 'org_id': 'org_acme'})
    assert r.status_code == 200, r.text
    token = parse_qs(urlparse(r.json()['dev_verify_url']).query)['verify_token'][0]
    assert requests.get(f'{B}/api/v1/auth/verify-email', params={'token': token}).status_code == 200
    open(path, 'w').write(f'{email}\n{pw}\n')

def main():
    tmp = tempfile.mkdtemp()
    creds = os.path.join(tmp, 'creds.txt')
    if sys.argv[1:] == ['--signup']:
        signup(creds)
    else:
        creds = sys.argv[1]
    fresh_path = os.path.join(tmp, 'server.json')
    subprocess.run([sys.executable, os.path.join(HERE, 'run_server.py'), creds, fresh_path], check=True)
    fresh = {t['id']: t for t in json.load(open(fresh_path))}
    docs = {t['id']: t for t in json.load(open(os.path.join(HERE, 'tutorials.json')))}
    problems = []
    for tid, t in fresh.items():
        old = docs.get(tid)
        if not old:
            problems.append(f'{tid}: not in tutorials.json')
            continue
        if len(old['cases']) != len(t['cases']):
            problems.append(f'{tid}: {len(old["cases"])} cases documented, {len(t["cases"])} now')
        for a, b in zip(old['cases'], t['cases']):
            if a.get('status') != b.get('status'):
                problems.append(f'{tid} / {a["title"]}: status {a.get("status")} documented, {b.get("status")} now')
            elif shape(a.get('response')) != shape(b.get('response')):
                problems.append(f'{tid} / {a["title"]}: response changed\n  documented: {json.dumps(shape(a.get("response")))}\n  now:        {json.dumps(shape(b.get("response")))}')
    if problems:
        print('The tutorials no longer match the server:\n' + '\n'.join(problems))
        print('\nIf the change is intended, re-record them: infra/tutorials/README.md')
        sys.exit(1)
    print(f'tutorials match the server: {sum(len(t["cases"]) for t in fresh.values())} cases in {len(fresh)} tutorials')

main()

// Approval link page (/approve#<token>). Opening the page never decides
// anything: only a click on Approve or Deny does, so link previews in
// chat apps and mail scanners can't approve a request. All text goes in
// with textContent, never innerHTML.
(function () {
  const token = location.hash.slice(1);
  const $ = (id) => document.getElementById(id);
  const msg = (text, cls) => { $('msg').textContent = text; $('msg').className = 'msg ' + (cls || ''); };

  async function call(path, body) {
    const res = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(Object.assign({ token }, body || {})) });
    let data = {};
    try { data = await res.json(); } catch (e) { /* empty or non-JSON answer */ }
    if (!res.ok) throw new Error(data.error || data.message || 'Something went wrong (' + res.status + ').');
    return data;
  }

  function show(r) {
    $('d-action').textContent = r.service + '.' + r.action;
    $('d-org').textContent = r.org_id;
    $('d-time').textContent = new Date(r.created_at).toLocaleString();
    $('d-status').textContent = r.status;
    $('d-payload').textContent = JSON.stringify(r.payload, null, 2);
    $('details').hidden = false;
    const pending = r.status === 'pending';
    $('buttons').hidden = !pending;
    $('title').textContent = pending ? 'Approve this agent action?' : 'This request is ' + r.status;
  }

  async function decide(decision) {
    $('approve-btn').disabled = $('deny-btn').disabled = true;
    msg(decision === 'approve' ? 'Approving and running the action...' : 'Denying...');
    try {
      const r = await call('/v1/hitl/link/decide', { decision });
      $('buttons').hidden = true;
      $('d-status').textContent = r.status;
      $('title').textContent = 'This request is ' + r.status;
      if (r.status === 'approved') msg('Approved. The action ran.', 'ok');
      else if (r.status === 'denied') msg('Denied. The action did not run.', 'ok');
      else msg('Approved, but the action failed. See Failed requests in the dashboard.', 'bad');
    } catch (e) {
      msg(e.message, 'bad');
      $('approve-btn').disabled = $('deny-btn').disabled = false;
    }
  }

  $('approve-btn').addEventListener('click', () => decide('approve'));
  $('deny-btn').addEventListener('click', () => decide('deny'));

  if (!token) {
    $('title').textContent = 'Link incomplete';
    msg('This link is missing its code. Open it again from the message you received.', 'bad');
    return;
  }
  call('/v1/hitl/link').then(show).catch((e) => { $('title').textContent = 'Link not valid'; msg(e.message, 'bad'); });
})();

# Docs tutorials

The Tutorials part of `/docs` and its videos (`public/videos/tutorial-*.mp4`)
are generated from real runs against a local server, so every request and
response shown is one the server actually gave.

1. Local stack up (`./install.sh`), a verified test account in `org_acme`
   (email and password on two lines in a creds file), Python with
   `pip install "agentraas[apply,langchain]" playwright requests`.
2. `python run_server.py creds.txt server.json`: connect, retries, validation,
   approvals, spend caps, policies, failures, undo, record/replay. It clears
   this account's rules and the local Redis dedup/cap/breaker keys first, so
   it can be re-run (local stack only, never production).
3. `python run_tools.py <venv bin> creds.txt work tools.json`: rules as code,
   library mode and chaos tester, LangChain, MCP scan and wrap.
4. `python run_dashboard.py creds.txt ../../src/api-gateway-rs/public/videos dashboard.json`:
   the same features clicked through in the real dashboard (headless Chromium,
   one screenshot per step plus a screen recording per tutorial). Stripe
   examples use a made-up `sk_test_` key, so only the Failures tour lets a call
   reach Stripe (and fail, which is the point there).
5. Merge the server and tools outputs (steps 2 and 3) into `tutorials.json` (server first), then
   `python3 build_docs.py` (a second run must say "nothing to change").
6. Videos: `python gen_video.py base.css.html tutorials.json pages`, then per
   tutorial `python render.py $PWD/pages/<id>.html video <seconds from
   pages/durations.json> ../../src/api-gateway-rs/public/videos/tutorial-<id>.mp4`
   (headless Chromium from Playwright's cache plus ffmpeg, 60 fps), and a
   poster: `ffmpeg -ss 4 -i tutorial-<id>.mp4 -frames:v 1 -vf scale=1280:-1 tutorial-<id>.jpg`.
7. `infra/landing-worker/deploy.sh` publishes the page and the videos.

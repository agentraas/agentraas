import sys, glob, subprocess
from playwright.sync_api import sync_playwright
exe = glob.glob('/home/sumedh/.cache/ms-playwright/chromium-1223/*/chrome')[0]
page, mode = sys.argv[1], sys.argv[2]
with sync_playwright() as p:
    b = p.chromium.launch(executable_path=exe)
    pg = b.new_page(viewport={'width': 1920, 'height': 1080})
    pg.goto('file://' + page); pg.evaluate('document.fonts.ready'); pg.wait_for_timeout(1500); pg.evaluate('seek(0)')
    if mode == 'stills':
        for t in map(float, sys.argv[4:]):
            pg.evaluate(f'seek({t})'); pg.screenshot(path=f'{sys.argv[3]}-{t:05.1f}.png')
    else:
        total, out, fps = float(sys.argv[3]), sys.argv[4], 60
        ff = subprocess.Popen(['ffmpeg', '-y', '-loglevel', 'error', '-f', 'image2pipe', '-framerate', str(fps), '-c:v', 'png', '-i', '-',
              '-c:v', 'libx264', '-preset', 'slow', '-crf', '21', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', out], stdin=subprocess.PIPE)
        for i in range(int(total * fps)):
            pg.evaluate(f'seek({i / fps})'); ff.stdin.write(pg.screenshot(type='png'))
        ff.stdin.close(); ff.wait()
    b.close()

#!/usr/bin/env python3
"""Record the real CLI against isolated sample APIs and render a GitHub-friendly GIF.

Requires a built target/debug/lunchmoney, Python 3 with Pillow, and a Menlo or
DejaVu Sans Mono font. Run from any directory: python3 docs/record-demo.py.
No real credentials, account data, or external AI services are used.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import textwrap
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/lunchmoney'
CATEGORIES = [dict(id=11, name='Dining'), dict(id=12, name='Groceries'),
              dict(id=13, name='Subscriptions'), dict(id=30, name='Uncategorized')]
TRANSACTIONS = [dict(id=101, date='2026-10-02', payee='SQ *CAFE KOO', amount='32.98',
                    currency='cad', category_id=30, plaid_account_id=1, status='unreviewed',
                    original_name='SQ *CAFE KOO', notes='Dinner with friends', updated_at='demo-v1',
                    plaid_metadata={'merchant_name': 'Cafe Koo'}),
                dict(id=102, date='2026-10-03', payee='Metro', amount='68.42', currency='cad',
                     category_id=12, plaid_account_id=1, status='reviewed', updated_at='demo-v1'),
                dict(id=103, date='2026-10-04', payee='Spotify', amount='12.99', currency='cad',
                     category_id=13, plaid_account_id=1, status='reviewed', updated_at='demo-v1')]
WRITES = []

class SampleAPI(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, value):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

    def do_GET(self):
        path = self.path.split('?')[0]
        if path == '/v2/transactions':
            # Review history is deliberately empty, keeping the example concise.
            items = [] if 'limit=1000' in self.path else TRANSACTIONS
            self.reply({'transactions': items, 'has_more': False})
        elif path.startswith('/v2/transactions/'):
            self.reply(next(tx for tx in TRANSACTIONS if tx['id'] == int(path.rsplit('/', 1)[1])))
        elif path == '/v2/categories':
            self.reply({'categories': CATEGORIES})
        elif path == '/v2/plaid_accounts':
            self.reply({'plaid_accounts': [{'id': 1, 'display_name': 'Chequing'}]})
        elif path == '/v2/manual_accounts':
            self.reply({'manual_accounts': []})
        elif path == '/v2/tags':
            self.reply({'tags': []})
        elif path == '/v2/me':
            self.reply({'primary_currency': 'cad'})
        elif path == '/v2/summary':
            rows = [(11, 250, 182.98, 67.02), (12, 500, 321.42, 178.58), (13, 60, 52.99, 7.01)]
            self.reply({'categories': [dict(category_id=i, totals=dict(budgeted=b,
                       other_activity=s, recurring_activity=0, available=a)) for i,b,s,a in rows]})
        else:
            self.send_error(404)

    def do_POST(self):
        assert self.path == '/ai/chat/completions'
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert not body.get('stream'), 'Demo review expects a structured batch response'
        content = json.dumps({'suggestions': [dict(transaction_id=101, category_id=11,
                  payee='Cafe Koo', reason='Bank merchant name and dinner note point to Dining.',
                  confidence='high')]})
        self.reply({'id': 'demo', 'object': 'chat.completion', 'model': 'demo',
                    'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': content},
                                 'finish_reason': 'stop'}]})

    def do_PUT(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert self.path == '/v2/transactions/101'
        assert body == {'category_id': 11, 'payee': 'Cafe Koo'}
        WRITES.append(body)
        self.reply(dict(TRANSACTIONS[0], **body))

ANSI = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')

def capture(args, env, accept=False):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, __import__('termios').TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
    process = subprocess.Popen([str(BINARY), *args], stdin=slave, stdout=slave, stderr=slave, env=env)
    os.close(slave)
    output, answered = b'', False
    deadline = time.monotonic() + 20
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], .1)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                output += chunk
                if accept and not answered and b'[q] quit: ' in output:
                    os.write(master, b'a\n')
                    answered = True
            elif process.poll() is not None:
                break
        process.wait(timeout=2)
        text = ANSI.sub('', output.decode()).replace('\r\n', '\n').replace('\r', '')
        assert process.returncode == 0, text
        if accept:
            assert answered and WRITES and 'Saved.' in text, text
        return text
    finally:
        if process.poll() is None:
            process.kill()
        os.close(master)


def record():
    server = HTTPServer(('127.0.0.1', 0), SampleAPI)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='lunchmoney-demo-') as config:
            env = {k:v for k,v in os.environ.items() if not k.startswith('LUNCH_MONEY')
                   and k not in ('OPENAI_API_KEY', 'XDG_CONFIG_HOME')}
            base = f'http://127.0.0.1:{server.server_port}'
            env.update(XDG_CONFIG_HOME=config, COLUMNS='100', TERM='xterm-256color',
                       LUNCH_MONEY_TOKEN='sample-token', LUNCH_MONEY_BASE_URL=base+'/v2',
                       LUNCH_MONEY_AI_PROVIDER='openai', LUNCH_MONEY_AI_MODEL='demo',
                       LUNCH_MONEY_AI_API_KEY='sample-ai-key', LUNCH_MONEY_AI_BASE_URL=base+'/ai')
            return [capture(['transactions', 'list', '--month', '2026-10'], env),
                    capture(['budgets', 'view', '--month', '2026-10'], env),
                    capture(['transactions', 'review', '--month', '2026-10', '--no-web-search'], env, True)]
    finally:
        server.shutdown()
        server.server_close()


def render(outputs):
    font_paths = ['/System/Library/Fonts/Menlo.ttc', '/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf']
    font_path = next((p for p in font_paths if Path(p).exists()), None)
    if font_path is None:
        raise RuntimeError('Install Menlo or DejaVu Sans Mono to render the demo')
    font = ImageFont.truetype(font_path, 17)
    small = ImageFont.truetype(font_path, 14)
    W,H = 1120,660
    bg,fg,muted,green = '#101820','#d9e4ed','#8197aa','#77ddac'
    frames,durations = [],[]
    events=[]
    elapsed=0

    def frame(title, lines, duration, cursor=False):
        nonlocal elapsed
        im=Image.new('RGB',(W,H),'#091017')
        d=ImageDraw.Draw(im)
        d.rounded_rectangle((12,12,W-12,H-12),radius=16,fill=bg,outline='#263746',width=1)
        d.rounded_rectangle((12,12,W-12,62),radius=16,fill='#1b2834')
        d.rectangle((13,40,W-13,62),fill='#1b2834')
        for x,col in [(35,'#ff6b6b'),(55,'#ffd166'),(75,'#77ddac')]:
            d.ellipse((x-5,32,x+5,42),fill=col)
        d.text((100,27),'lunchmoney',font=small,fill=fg)
        d.text((W-225,27),'DEMO / SAMPLE DATA',font=small,fill=muted)
        d.text((35,81),title,font=small,fill=green)
        wrapped=[]
        for line in lines:
            wrapped.extend(textwrap.wrap(line, width=104, replace_whitespace=False,
                           break_on_hyphens=False) if len(line)>104 else [line])
        visible=wrapped[-21:]
        for i,line in enumerate(visible):
            color=green if line.startswith('$') or line.startswith('Saved.') else fg
            if 'Cafe Koo' in line and 'Dining' in line:
                color=green
            if line.startswith('─') or line.startswith('Usage =') or 'transactions' == line.strip().split(' ')[-1]:
                color=muted
            d.text((35,118+i*23),line,font=font,fill=color)
        if cursor and visible:
            x=35+d.textlength(visible[-1],font=font)
            y=118+(len(visible)-1)*23
            d.rectangle((x+2,y+3,x+10,y+20),fill=green)
        frames.append(im)
        durations.append(duration)
        elapsed+=duration/1000

    scenes=[('01 / Browse your transactions', 'lunchmoney transactions list --month 2026-10',outputs[0]),
            ('02 / See where your budget stands', 'lunchmoney budgets view --month 2026-10',outputs[1]),
            ('03 / Review suggestions. Accept the changes you want.',
             'lunchmoney transactions review --month 2026-10 --no-web-search',outputs[2])]
    for title,command,output in scenes:
        frame(title,['$ '],400,True)
        for i in range(3,len(command)+3,3):
            frame(title,['$ '+command[:i]],65,True)
        lines=['$ '+command,'']
        for line in output.rstrip().split('\n'):
            if '[q] quit: a' in line:
                # Pause on the actual acceptance prompt before showing the keypress.
                pending=line.rsplit('a',1)[0]
                frame(title,lines+[pending],4500,True)
            lines.append(line)
            frame(title,lines,130)
        frame(title,lines,3500)
        events.append((elapsed,command,output))
    palette=frames[-2].quantize(colors=128)
    indexed=[im.quantize(palette=palette,dither=Image.Dither.NONE) for im in frames]
    indexed[0].save(ROOT/'docs/demo.gif',save_all=True,append_images=indexed[1:],
                   duration=durations,loop=0,optimize=True,disposal=1)
    # Keep an accessible still showing the budget screen for inspection.
    title,cmd,out=scenes[1]
    frame(title,['$ '+cmd,'']+out.rstrip().split('\n'),100)
    frames[-1].save(ROOT/'docs/demo-poster.png')
    transcript='\n\n'.join('$ '+cmd+'\n'+out for _,cmd,out in scenes)
    (ROOT/'docs/demo-transcript.txt').write_text('\n'.join(line.rstrip() for line in transcript.splitlines())+'\n')
    print(f'Recorded {len(frames)-1} frames, {elapsed:.1f}s, {(ROOT/"docs/demo.gif").stat().st_size:,} bytes')

if __name__ == '__main__':
    render(record())

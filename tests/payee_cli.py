"""Run after cargo build. Tests payee filtering across API pages with dummy credentials."""
import json
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

requests = []
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        assert self.headers['Authorization'] == 'Bearer dummy-key'
        url = urlparse(self.path)
        query = parse_qs(url.query)
        requests.append((url.path, query))
        if url.path == '/v2/categories':
            value = {'categories':[{'id':30,'name':'Shopping'}]}
        else:
            assert url.path == '/v2/transactions'
            assert 'payee' not in query
            assert query['category_id'] == ['30']
            assert query['start_date'] == ['2026-10-01'] and query['end_date'] == ['2026-10-31']
            offset = int(query['offset'][0])
            value = {'transactions':[{'id':offset+1,'payee':['Other Merchant','LS Store','ls cafe'][offset]}],
                     'has_more':offset < 2}
        self.send_response(200)
        self.send_header('Content-Type','application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

server = HTTPServer(('127.0.0.1',0), Handler)
threading.Thread(target=server.serve_forever,daemon=True).start()
binary = Path(__file__).resolve().parents[1] / 'target/debug/lunchmoney'
try:
    for text, expected in [('Ls',[2,3]), (' store ',[2]), ('missing',[])]:
        requests.clear()
        result = subprocess.run([str(binary),'--token','dummy-key','--base-url',f'http://127.0.0.1:{server.server_port}/v2',
            'transactions','list','--month','2026-10','--category','shopping','--payee',text,'--limit','1','--offset','99'],
            capture_output=True,text=True,check=True)
        assert [tx['id'] for tx in json.loads(result.stdout)['transactions']] == expected
        assert [q['offset'][0] for path,q in requests if path.endswith('/transactions')] == ['0','1','2']
    result = subprocess.run([str(binary),'--token','dummy-key','transactions','list','--payee',' '],capture_output=True,text=True)
    assert result.returncode and '--payee must not be empty' in result.stderr
    help_result = subprocess.run([str(binary),'transactions','review','--category','shopping','--payee','Ls','--help'],capture_output=True,text=True,check=True)
    assert '--payee <TEXT>' in help_result.stdout
    print('Payee filtering checks passed: category combination, case-insensitive substring, later pages, whitespace, no matches, empty input, review flag, and offset handling.')
finally:
    server.shutdown()
    server.server_close()
